use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandle,
    GetFileInformationByHandleEx,
};

use mlua::{Function, Lua, MultiValue, Table, UserData, UserDataMethods, Value as LuaValue};

use crate::runtime::encoding::{RuntimeTextEncoding, decode_runtime_text, encode_runtime_text};
use crate::runtime::path::normalize_host_input_path_text;
use crate::runtime::process_session::{ManagedChildProcessTree, finalize_one_shot_process_tree};

/// Process-local monotonic suffix used to reserve managed temporary file names.
/// 用于预留托管临时文件名的进程内单调后缀。
static TMPFILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Managed file open mode supported by the first Rust-backed IO layer.
/// 第一版 Rust 托管 IO 层支持的文件打开模式。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManagedIoModeKind {
    /// Read existing file content.
    /// 读取已有文件内容。
    Read,
    /// Truncate and write file content.
    /// 截断并写入文件内容。
    Write,
    /// Append content to the end of an existing or new file.
    /// 将内容追加到已有或新建文件末尾。
    Append,
}

/// Parsed managed IO open mode.
/// 解析后的托管 IO 打开模式。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ManagedIoOpenMode {
    /// High-level access behavior.
    /// 高层访问行为。
    kind: ManagedIoModeKind,
    /// Whether read/write operations should preserve raw Lua string bytes.
    /// 读写操作是否应保留 Lua 字符串原始字节。
    binary: bool,
    /// Whether this handle supports both reading and writing.
    /// 此句柄是否同时支持读取与写入。
    update: bool,
}

/// Mutable state behind one managed IO file handle.
/// 单个托管 IO 文件句柄背后的可变状态。
struct ManagedIoFileState {
    /// Filesystem path owned by this handle.
    /// 此句柄拥有的文件系统路径。
    path: PathBuf,
    /// Access mode selected at open time.
    /// 打开时选择的访问模式。
    mode: ManagedIoOpenMode,
    /// Text encoding used by non-binary reads and writes.
    /// 非二进制读写使用的文本编码。
    encoding: RuntimeTextEncoding,
    /// In-memory read or write buffer.
    /// 内存中的读取或写入缓冲区。
    buffer: Vec<u8>,
    /// Current read cursor inside the buffer.
    /// 缓冲区内的当前读取游标。
    cursor: usize,
    /// Number of buffer bytes represented by the last successful flush.
    /// 上次成功刷新后已由磁盘表示的缓冲区字节数。
    flushed_len: usize,
    /// Whether the writable backing file has completed its initial create/truncate publication.
    /// 可写底层文件是否已经完成首次创建或截断发布。
    flush_initialized: bool,
    /// Cheap metadata fingerprint used to recognize an unchanged backing file before every incremental flush.
    /// 用于避免每次增量刷新前重复处理未变化底层文件的低成本元数据指纹。
    flushed_fingerprint: Option<ManagedIoBackingFingerprint>,
    /// Smallest contiguous buffer range covering all unflushed update-mode writes.
    /// 覆盖所有尚未刷新更新模式写入的最小连续缓冲区范围。
    dirty_range: Option<(usize, usize)>,
    /// Successful physical payload bytes written by flush operations in tests.
    /// 测试中刷新操作成功写入的物理载荷字节数。
    #[cfg(test)]
    physical_write_bytes: usize,
    /// Whether this handle has already been closed.
    /// 此句柄是否已经关闭。
    closed: bool,
    /// Whether the backing file should be removed when the handle closes.
    /// 句柄关闭时是否移除底层文件。
    delete_on_close: bool,
    /// Optional process status returned when this handle was created by popen.
    /// 当此句柄由 popen 创建时返回的可选进程状态。
    close_status: Option<ManagedIoCloseStatus>,
}

/// Cheap backing-file generation fingerprint used before incremental publication.
/// 增量发布前使用的低成本底层文件代指纹。
#[derive(Clone, Debug, Eq, PartialEq)]
struct ManagedIoBackingFingerprint {
    /// Current file length in bytes.
    /// 当前文件字节长度。
    len: u64,
    /// Last modification timestamp when the filesystem exposes one.
    /// 文件系统能够提供时的最后修改时间戳。
    modified: Option<SystemTime>,
    /// Creation timestamp when the filesystem exposes one, allowing replacement detection.
    /// 文件系统能够提供时的创建时间戳，用于检测文件替换。
    created: Option<SystemTime>,
    /// Platform file identity and change generation when the host exposes reliable values.
    /// 宿主能够提供可靠值时的平台文件身份与变更代。
    platform_generation: Option<ManagedIoPlatformGeneration>,
}

impl ManagedIoBackingFingerprint {
    /// Build one cheap generation fingerprint from an exact open file handle.
    /// 从一个精确打开的文件句柄构造低成本代指纹。
    ///
    /// The file parameter identifies the exact open object whose observable generation is validated.
    /// file 参数标识需要校验可观察变更代的精确打开对象。
    ///
    /// Returns length, portable timestamps, and platform change-generation data.
    /// 返回文件长度、可移植时间戳与平台变更代数据。
    fn from_file(file: &File) -> std::io::Result<Self> {
        // Metadata is tied to the same handle used for platform generation collection.
        // Metadata 绑定到用于采集平台变更代的同一个句柄。
        let metadata = file.metadata()?;
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
            platform_generation: managed_io_platform_generation(file, &metadata)?,
        })
    }
}

/// Platform file identity and content-change generation used without reading the payload.
/// 无需读取载荷即可使用的平台文件身份与内容变更代。
#[derive(Clone, Debug, Eq, PartialEq)]
struct ManagedIoPlatformGeneration {
    /// Filesystem or volume identity owning the file.
    /// 拥有文件的文件系统或卷标识。
    storage_id: u64,
    /// Stable file identity inside the owning filesystem or volume.
    /// 文件在所属文件系统或卷内的稳定标识。
    file_id: u64,
    /// Platform content-change time in seconds or native ticks.
    /// 以秒或平台原生 tick 表示的内容变更时间。
    change_time: i64,
    /// Subsecond component used by Unix ctime; zero for tick-based platforms.
    /// Unix ctime 使用的亚秒分量；基于 tick 的平台为零。
    change_time_subsecond: i64,
}

/// Read platform file identity and change generation from one exact open handle.
/// 从一个精确打开的句柄读取平台文件身份与变更代。
///
/// The file parameter identifies the handle whose generation must be captured.
/// file 参数标识必须捕获变更代的句柄。
///
/// The metadata parameter belongs to the same handle and supplies portable file identity fields.
/// metadata 参数属于同一句柄，并提供可移植文件身份字段。
///
/// Returns a platform generation when supported, otherwise no generation on unsupported targets.
/// 在受支持平台返回平台变更代；不支持的平台返回空值。
#[cfg(windows)]
fn managed_io_platform_generation(
    file: &File,
    _metadata: &Metadata,
) -> std::io::Result<Option<ManagedIoPlatformGeneration>> {
    // BasicInfo receives the NT change time, which changes for same-length in-place rewrites.
    // BasicInfo 接收 NT 变更时间，该时间会在同长度原地重写时变化。
    let mut basic_info = FILE_BASIC_INFO::default();
    // SAFETY: RawHandle remains valid for the call, BasicInfo is writable and its exact byte length is supplied.
    // 安全性：RawHandle 在调用期间保持有效，BasicInfo 可写且传入了其精确字节长度。
    let succeeded = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            std::ptr::from_mut(&mut basic_info).cast(),
            u32::try_from(std::mem::size_of::<FILE_BASIC_INFO>())
                .expect("FILE_BASIC_INFO size must fit in u32"),
        )
    };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // HandleInfo receives stable volume and file identifiers for replacement detection.
    // HandleInfo 接收稳定的卷与文件标识，用于检测文件替换。
    let mut handle_info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: RawHandle remains valid for the call and HandleInfo is a writable exact-layout output buffer.
    // 安全性：RawHandle 在调用期间保持有效，HandleInfo 是可写且布局精确的输出缓冲区。
    let succeeded = unsafe {
        GetFileInformationByHandle(file.as_raw_handle(), std::ptr::from_mut(&mut handle_info))
    };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // FileId combines the documented high and low halves without lossy conversion.
    // FileId 无损合并文档定义的高低两部分。
    let file_id =
        (u64::from(handle_info.nFileIndexHigh) << 32) | u64::from(handle_info.nFileIndexLow);
    Ok(Some(ManagedIoPlatformGeneration {
        storage_id: u64::from(handle_info.dwVolumeSerialNumber),
        file_id,
        change_time: basic_info.ChangeTime,
        change_time_subsecond: 0,
    }))
}

/// Read platform file identity and change generation from one exact open handle.
/// 从一个精确打开的句柄读取平台文件身份与变更代。
///
/// The file parameter keeps the cross-platform signature aligned and is unused on Unix.
/// file 参数用于保持跨平台签名一致，在 Unix 上不使用。
///
/// The metadata parameter supplies Unix device, inode, and nanosecond ctime values.
/// metadata 参数提供 Unix 设备、inode 与纳秒级 ctime 值。
///
/// Returns the Unix file identity and content-change generation.
/// 返回 Unix 文件身份与内容变更代。
#[cfg(unix)]
fn managed_io_platform_generation(
    _file: &File,
    metadata: &Metadata,
) -> std::io::Result<Option<ManagedIoPlatformGeneration>> {
    Ok(Some(ManagedIoPlatformGeneration {
        storage_id: metadata.dev(),
        file_id: metadata.ino(),
        change_time: metadata.ctime(),
        change_time_subsecond: metadata.ctime_nsec(),
    }))
}

/// Return no platform generation on targets without a supported identity API.
/// 在没有受支持身份 API 的目标平台上不返回平台变更代。
///
/// The file and metadata parameters keep the portable call surface uniform.
/// file 与 metadata 参数用于保持可移植调用界面统一。
///
/// Returns no generation so callers safely fall back to full publication.
/// 返回空变更代，使调用方安全回退到完整发布。
#[cfg(not(any(unix, windows)))]
fn managed_io_platform_generation(
    _file: &File,
    _metadata: &Metadata,
) -> std::io::Result<Option<ManagedIoPlatformGeneration>> {
    Ok(None)
}

/// Read one update-capable buffer and its generation from the same stable file handle.
/// 从同一稳定文件句柄读取支持更新的缓冲区及其变更代。
///
/// The path parameter identifies the existing file that becomes the in-memory authority.
/// path 参数标识将成为内存权威来源的已有文件。
///
/// Returns the complete bytes and their observable before/after generation, or an error if the host reports a change during the read.
/// 返回完整字节及其可观察的前后变更代；若宿主报告读取期间发生变化则返回错误。
fn read_managed_io_update_buffer(
    path: &Path,
) -> std::io::Result<(Vec<u8>, ManagedIoBackingFingerprint)> {
    // File is retained across reading and both generation captures to prevent path-replacement ambiguity.
    // File 在读取及两次变更代捕获期间保持打开，避免路径替换歧义。
    let mut file = File::open(path)?;
    // BeforeFingerprint captures the generation whose bytes are about to be read.
    // BeforeFingerprint 捕获即将读取字节所属的变更代。
    let before_fingerprint = ManagedIoBackingFingerprint::from_file(&file)?;
    // Buffer receives the complete authoritative starting content.
    // Buffer 接收完整的初始权威内容。
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;
    // AfterFingerprint rejects concurrent in-place changes during the initial read.
    // AfterFingerprint 拒绝初始读取期间发生的并发原地变更。
    let after_fingerprint = ManagedIoBackingFingerprint::from_file(&file)?;
    if before_fingerprint != after_fingerprint {
        return Err(std::io::Error::other(format!(
            "managed file changed while opening {}",
            path.display()
        )));
    }
    Ok((buffer, after_fingerprint))
}

/// Process close status retained for a managed popen read handle.
/// 托管 popen 读取句柄保留的进程关闭状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ManagedIoCloseStatus {
    /// Whether the spawned process exited successfully.
    /// 启动的进程是否成功退出。
    success: bool,
}

/// Rust-backed file handle exposed to Lua.
/// 暴露给 Lua 的 Rust 托管文件句柄。
#[derive(Clone)]
struct ManagedIoFile {
    /// Shared mutable handle state protected from aliasing across Lua calls.
    /// 跨 Lua 调用共享且受保护的可变句柄状态。
    state: Arc<Mutex<ManagedIoFileState>>,
}

/// Mutable compatibility state for the Lua standard `io` table facade.
/// Lua 标准 `io` 表兼容外观使用的可变状态。
struct ManagedIoCompatState {
    /// Current default input file used by `io.read`.
    /// `io.read` 使用的当前默认输入文件。
    current_input: Option<ManagedIoFile>,
    /// Current default output file used by `io.write` and `io.flush`.
    /// `io.write` 与 `io.flush` 使用的当前默认输出文件。
    current_output: Option<ManagedIoFile>,
}

/// Runtime options captured by one Rust-backed managed IO table.
/// 单个 Rust 托管 IO 表捕获的运行时选项。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ManagedIoOptions {
    /// Default text encoding used when a Lua call omits explicit encoding options.
    /// Lua 调用未显式提供编码选项时使用的默认文本编码。
    default_encoding: RuntimeTextEncoding,
}

impl ManagedIoFile {
    /// Open one managed file handle from a normalized request.
    /// 根据归一化请求打开一个托管文件句柄。
    fn open(
        path: PathBuf,
        mode: ManagedIoOpenMode,
        encoding: RuntimeTextEncoding,
    ) -> mlua::Result<Self> {
        // Initial buffer, synchronized length, and publication state derived from the exact open mode.
        // 从精确打开模式派生的初始缓冲区、同步长度和发布状态。
        let (buffer, flushed_len, flush_initialized, flushed_fingerprint) =
            match (mode.kind, mode.update) {
                (ManagedIoModeKind::Read, true) => {
                    // Existing bytes and observable generation required by read-update mode.
                    // 读更新模式所需的已有字节与可观察变更代。
                    let (bytes, fingerprint) =
                        read_managed_io_update_buffer(&path).map_err(|error| {
                            mlua::Error::runtime(format!("vulcan.io.open: {error}"))
                        })?;
                    // Persisted length matches the complete buffer loaded from the stable handle.
                    // 已落盘长度与从稳定句柄加载的完整缓冲区一致。
                    let persisted_len = bytes.len();
                    (bytes, persisted_len, true, Some(fingerprint))
                }
                (ManagedIoModeKind::Read, false) => {
                    // Existing bytes required by read-only mode.
                    // 只读模式所需的已有字节。
                    let bytes = fs::read(&path).map_err(|error| {
                        mlua::Error::runtime(format!("vulcan.io.open: {error}"))
                    })?;
                    // Persisted length matches the complete buffer loaded from disk.
                    // 已落盘长度与从磁盘加载的完整缓冲区一致。
                    let persisted_len = bytes.len();
                    (bytes, persisted_len, true, None)
                }
                (ManagedIoModeKind::Append, true) => match read_managed_io_update_buffer(&path) {
                    Ok((bytes, fingerprint)) => {
                        // Persisted length matches the existing content retained by append-update.
                        // 已落盘长度与追加更新模式保留的已有内容一致。
                        let persisted_len = bytes.len();
                        (bytes, persisted_len, true, Some(fingerprint))
                    }
                    Err(error) if error.kind() == ErrorKind::NotFound => {
                        // Missing append-update target requires initial publication on first flush.
                        // 缺失的追加更新目标需要在首次刷新时完成初始发布。
                        (Vec::new(), 0, false, None)
                    }
                    Err(error) => {
                        return Err(mlua::Error::runtime(format!("vulcan.io.open: {error}")));
                    }
                },
                (ManagedIoModeKind::Write, _) => {
                    // Write modes begin from an empty authoritative buffer and publish by truncation.
                    // 写入模式从空的权威缓冲区开始，并通过截断完成发布。
                    (Vec::new(), 0, false, None)
                }
                (ManagedIoModeKind::Append, false) => {
                    // Append-only buffers contain only bytes owned by this handle.
                    // 纯追加缓冲区只包含当前句柄拥有的字节。
                    (Vec::new(), 0, true, None)
                }
            };
        Ok(Self {
            state: Arc::new(Mutex::new(ManagedIoFileState {
                path,
                mode,
                encoding,
                buffer,
                cursor: 0,
                flushed_len,
                flush_initialized,
                flushed_fingerprint,
                dirty_range: None,
                #[cfg(test)]
                physical_write_bytes: 0,
                closed: false,
                delete_on_close: false,
                close_status: None,
            })),
        })
    }

    /// Create one update-capable temporary file handle that deletes its backing file on close.
    /// 创建一个支持更新读写并在关闭时删除底层文件的临时文件句柄。
    fn tmpfile(encoding: RuntimeTextEncoding) -> mlua::Result<Self> {
        // Reserved empty file created atomically before the managed handle is published.
        // 在托管句柄发布前以原子方式创建的已预留空文件。
        let path = reserve_tmpfile_path()?;
        // FlushedFingerprint identifies the atomically reserved empty backing file.
        // FlushedFingerprint 标识以原子方式预留的空底层文件。
        let flushed_fingerprint = File::open(&path)
            .and_then(|file| ManagedIoBackingFingerprint::from_file(&file))
            .map_err(|error| mlua::Error::runtime(format!("vulcan.io.tmpfile: {error}")))?;
        Ok(Self {
            state: Arc::new(Mutex::new(ManagedIoFileState {
                path,
                mode: ManagedIoOpenMode {
                    kind: ManagedIoModeKind::Write,
                    binary: false,
                    update: true,
                },
                encoding,
                buffer: Vec::new(),
                cursor: 0,
                flushed_len: 0,
                flush_initialized: true,
                flushed_fingerprint: Some(flushed_fingerprint),
                dirty_range: None,
                #[cfg(test)]
                physical_write_bytes: 0,
                closed: false,
                delete_on_close: true,
                close_status: None,
            })),
        })
    }

    /// Create one read-only managed handle from already captured bytes.
    /// 从已经捕获的字节创建一个只读托管句柄。
    fn from_read_buffer(
        label: String,
        mode: ManagedIoOpenMode,
        encoding: RuntimeTextEncoding,
        buffer: Vec<u8>,
        close_status: Option<ManagedIoCloseStatus>,
    ) -> Self {
        // Read buffer length retained only for a complete initialized in-memory source.
        // 仅为完整初始化的内存读取源保留的读取缓冲区长度。
        let flushed_len = buffer.len();
        Self {
            state: Arc::new(Mutex::new(ManagedIoFileState {
                path: PathBuf::from(label),
                mode,
                encoding,
                buffer,
                cursor: 0,
                flushed_len,
                flush_initialized: true,
                flushed_fingerprint: None,
                dirty_range: None,
                #[cfg(test)]
                physical_write_bytes: 0,
                closed: false,
                delete_on_close: false,
                close_status,
            })),
        }
    }

    /// Return whether the managed file handle is closed.
    /// 返回托管文件句柄是否已经关闭。
    fn is_closed(&self) -> mlua::Result<bool> {
        let state = self.lock_state();
        Ok(state.closed)
    }

    /// Read values according to a limited Lua file:read-compatible format list.
    /// 按受限的 Lua file:read 兼容格式列表读取值。
    fn read_values(&self, lua: &Lua, formats: MultiValue) -> mlua::Result<MultiValue> {
        let mut output = MultiValue::new();
        let mut requested = formats.into_iter().peekable();
        if requested.peek().is_none() {
            output.push_back(self.read_one_line(lua)?);
            return Ok(output);
        }
        for format in requested {
            output.push_back(self.read_one(lua, format)?);
        }
        Ok(output)
    }

    /// Read one value from the managed file handle.
    /// 从托管文件句柄读取一个值。
    fn read_one(&self, lua: &Lua, format: LuaValue) -> mlua::Result<LuaValue> {
        match format {
            LuaValue::Nil => self.read_one_line(lua),
            LuaValue::String(text) => {
                let format_text = text
                    .to_str()
                    .map_err(|_| mlua::Error::runtime("file:read format must be valid UTF-8"))?;
                match format_text.as_ref() {
                    "*a" | "a" => self.read_all(lua),
                    "*l" | "l" => self.read_one_line(lua),
                    _ => Err(mlua::Error::runtime(format!(
                        "file:read unsupported format `{format_text}`"
                    ))),
                }
            }
            LuaValue::Integer(size) if size >= 0 => self.read_byte_count(lua, size as usize),
            LuaValue::Number(size) if size.is_finite() && size >= 0.0 && size.fract() == 0.0 => {
                self.read_byte_count(lua, size as usize)
            }
            other => Err(mlua::Error::runtime(format!(
                "file:read unsupported format argument {}",
                lua_value_type_name(&other)
            ))),
        }
    }

    /// Read all remaining content from the current cursor.
    /// 从当前游标读取全部剩余内容。
    fn read_all(&self, lua: &Lua) -> mlua::Result<LuaValue> {
        let mut state = self.lock_state();
        ensure_file_is_open(&state, "file:read")?;
        ensure_file_is_readable(&state, "file:read")?;
        let bytes = state.buffer[state.cursor..].to_vec();
        state.cursor = state.buffer.len();
        bytes_to_lua_value(lua, &bytes, state.mode.binary, state.encoding)
    }

    /// Read one line from the current cursor.
    /// 从当前游标读取一行。
    fn read_one_line(&self, lua: &Lua) -> mlua::Result<LuaValue> {
        let mut state = self.lock_state();
        ensure_file_is_open(&state, "file:read")?;
        ensure_file_is_readable(&state, "file:read")?;
        if state.cursor >= state.buffer.len() {
            return Ok(LuaValue::Nil);
        }
        let remaining = &state.buffer[state.cursor..];
        let line_end = remaining
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap_or(remaining.len());
        let mut next_cursor = state.cursor + line_end;
        let mut line = state.buffer[state.cursor..next_cursor].to_vec();
        if line.ends_with(b"\r") {
            line.pop();
        }
        if next_cursor < state.buffer.len() && state.buffer[next_cursor] == b'\n' {
            next_cursor += 1;
        }
        state.cursor = next_cursor;
        bytes_to_lua_value(lua, &line, state.mode.binary, state.encoding)
    }

    /// Read a fixed number of bytes from the current cursor.
    /// 从当前游标读取固定数量的字节。
    fn read_byte_count(&self, lua: &Lua, size: usize) -> mlua::Result<LuaValue> {
        let mut state = self.lock_state();
        ensure_file_is_open(&state, "file:read")?;
        ensure_file_is_readable(&state, "file:read")?;
        if size == 0 {
            return bytes_to_lua_value(lua, &[], state.mode.binary, state.encoding);
        }
        if state.cursor >= state.buffer.len() {
            return Ok(LuaValue::Nil);
        }
        let end = state.cursor.saturating_add(size).min(state.buffer.len());
        let bytes = state.buffer[state.cursor..end].to_vec();
        state.cursor = end;
        bytes_to_lua_value(lua, &bytes, state.mode.binary, state.encoding)
    }

    /// Write one or more Lua values into the managed file handle.
    /// 将一个或多个 Lua 值写入托管文件句柄。
    fn write_values(&self, values: MultiValue) -> mlua::Result<bool> {
        // Mutable handle state covering the complete logical write transaction.
        // 覆盖完整逻辑写入事务的可变句柄状态。
        let mut state = self.lock_state();
        ensure_file_is_open(&state, "file:write")?;
        ensure_file_is_writable(&state, "file:write")?;
        for value in values {
            // Encoded bytes for the current Lua value under this handle's mode and encoding.
            // 当前 Lua 值按本句柄模式和编码生成的字节。
            let bytes = lua_value_to_output_bytes(value, state.mode.binary, state.encoding)?;
            if state.mode.update {
                // Update write position follows append semantics or the explicit random-access cursor.
                // 更新写入位置遵循追加语义或显式随机访问游标。
                let write_position = if matches!(state.mode.kind, ManagedIoModeKind::Append) {
                    state.buffer.len()
                } else {
                    state.cursor
                };
                // Exclusive end of the logical update write.
                // 逻辑更新写入的排他结束位置。
                let write_end = write_position.saturating_add(bytes.len());
                if write_end > state.buffer.len() {
                    state.buffer.resize(write_end, 0);
                }
                state.buffer[write_position..write_end].copy_from_slice(&bytes);
                state.cursor = write_end;
                if write_position < write_end {
                    // Union range bounds every unflushed update without storing per-write intervals.
                    // 并集范围覆盖所有未刷新更新，无需保存逐次写入区间。
                    state.dirty_range = Some(match state.dirty_range {
                        Some((dirty_start, dirty_end)) => {
                            (dirty_start.min(write_position), dirty_end.max(write_end))
                        }
                        None => (write_position, write_end),
                    });
                }
            } else {
                state.buffer.extend_from_slice(&bytes);
                state.cursor = state.buffer.len();
            }
        }
        Ok(true)
    }

    /// Flush pending buffered writes to disk.
    /// 将挂起的缓冲写入刷新到磁盘。
    fn flush(&self) -> mlua::Result<bool> {
        let mut state = self.lock_state();
        ensure_file_is_open(&state, "file:flush")?;
        flush_state(&mut state)?;
        Ok(true)
    }

    /// Close this managed file handle and flush pending writes.
    /// 关闭此托管文件句柄并刷新挂起写入。
    fn close(&self) -> mlua::Result<bool> {
        let mut state = self.lock_state();
        if state.closed {
            return Ok(true);
        }
        flush_state(&mut state)?;
        if state.delete_on_close {
            match fs::remove_file(&state.path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(mlua::Error::runtime(format!(
                        "file:close: failed to remove temporary file: {error}"
                    )));
                }
            }
        }
        state.closed = true;
        Ok(state
            .close_status
            .map(|status| status.success)
            .unwrap_or(true))
    }

    /// Seek within the managed read buffer and return the new offset.
    /// 在托管读取缓冲区中移动游标并返回新偏移。
    fn seek(&self, whence: Option<String>, offset: Option<i64>) -> mlua::Result<i64> {
        let mut state = self.lock_state();
        ensure_file_is_open(&state, "file:seek")?;
        let base = match whence.as_deref().unwrap_or("cur") {
            "set" => 0_i64,
            "cur" => state.cursor as i64,
            "end" => state.buffer.len() as i64,
            other => {
                return Err(mlua::Error::runtime(format!(
                    "file:seek unsupported whence `{other}`"
                )));
            }
        };
        let next = base
            .checked_add(offset.unwrap_or(0))
            .ok_or_else(|| mlua::Error::runtime("file:seek offset overflow"))?;
        if next < 0 {
            return Err(mlua::Error::runtime("file:seek offset before start"));
        }
        state.cursor = (next as usize).min(state.buffer.len());
        Ok(state.cursor as i64)
    }

    /// Create one iterator function that reads lines until EOF.
    /// 创建一个逐行读取直到 EOF 的迭代器函数。
    fn lines(&self, lua: &Lua) -> mlua::Result<Function> {
        let file = self.clone();
        lua.create_function_mut(move |lua, ()| file.read_one_line(lua))
    }

    /// Lock the shared file state and return its guard, recovering after state lock poisoning.
    /// 锁定并返回共享文件状态保护对象；如果状态锁已 poison，则恢复继续使用。
    fn lock_state(&self) -> MutexGuard<'_, ManagedIoFileState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl UserData for ManagedIoFile {
    /// Register Lua-visible methods for the managed file handle.
    /// 为托管文件句柄注册 Lua 可见方法。
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("read", |lua, file, formats: MultiValue| {
            file.read_values(lua, formats)
        });
        methods.add_method("write", |_, file, values: MultiValue| {
            file.write_values(values)
        });
        methods.add_method("flush", |_, file, ()| file.flush());
        methods.add_method("close", |_, file, ()| file.close());
        methods.add_method(
            "seek",
            |_, file, (whence, offset): (Option<String>, Option<i64>)| file.seek(whence, offset),
        );
        methods.add_method("lines", |lua, file, ()| file.lines(lua));
        methods.add_method("setvbuf", |_, _file, _args: MultiValue| Ok(true));
    }
}

/// Build the Rust-backed `vulcan.io` Lua table.
/// 构建 Rust 托管的 `vulcan.io` Lua 表。
pub(crate) fn create_vulcan_io_table(
    lua: &Lua,
    default_encoding: RuntimeTextEncoding,
) -> mlua::Result<Table> {
    let options = ManagedIoOptions { default_encoding };
    let io_table = lua.create_table()?;
    let open_options = options;
    let open_fn =
        lua.create_function(move |lua, args: MultiValue| open_from_args(lua, args, open_options))?;
    let read_text_options = options;
    let read_text_fn = lua.create_function(move |lua, args: MultiValue| {
        read_text_from_args(lua, args, read_text_options)
    })?;
    let write_text_options = options;
    let write_text_fn = lua.create_function(move |_, args: MultiValue| {
        write_text_from_args(args, false, write_text_options)
    })?;
    let append_text_options = options;
    let append_text_fn = lua.create_function(move |_, args: MultiValue| {
        write_text_from_args(args, true, append_text_options)
    })?;
    let lines_options = options;
    let lines_fn = lua
        .create_function(move |lua, args: MultiValue| lines_from_args(lua, args, lines_options))?;
    let popen_options = options;
    let popen_fn = lua
        .create_function(move |lua, args: MultiValue| popen_from_args(lua, args, popen_options))?;
    let tmpfile_options = options;
    let tmpfile_fn = lua.create_function(move |lua, ()| tmpfile_from_args(lua, tmpfile_options))?;
    io_table.set("open", open_fn)?;
    io_table.set("read_text", read_text_fn)?;
    io_table.set("write_text", write_text_fn)?;
    io_table.set("append_text", append_text_fn)?;
    io_table.set("lines", lines_fn)?;
    io_table.set("popen", popen_fn)?;
    io_table.set("tmpfile", tmpfile_fn)?;
    Ok(io_table)
}

/// Install a Lua `io` compatibility table that forwards common calls to `vulcan.io`.
/// 安装一个 Lua `io` 兼容表，将常用调用转发到 `vulcan.io`。
pub(crate) fn install_managed_io_compat(
    lua: &Lua,
    vulcan_io: &Table,
    default_encoding: RuntimeTextEncoding,
) -> mlua::Result<()> {
    let options = ManagedIoOptions { default_encoding };
    let compat = lua.create_table()?;
    let compat_state = Arc::new(Mutex::new(ManagedIoCompatState {
        current_input: None,
        current_output: None,
    }));
    compat.set("open", vulcan_io.get::<Function>("open")?)?;
    compat.set("lines", vulcan_io.get::<Function>("lines")?)?;
    compat.set("popen", vulcan_io.get::<Function>("popen")?)?;
    compat.set("tmpfile", vulcan_io.get::<Function>("tmpfile")?)?;
    let input_state = compat_state.clone();
    let input_options = options;
    compat.set(
        "input",
        lua.create_function(move |lua, value: LuaValue| {
            set_or_get_compat_input(lua, input_state.clone(), value, input_options)
        })?,
    )?;
    let output_state = compat_state.clone();
    let output_options = options;
    compat.set(
        "output",
        lua.create_function(move |lua, value: LuaValue| {
            set_or_get_compat_output(lua, output_state.clone(), value, output_options)
        })?,
    )?;
    let read_state = compat_state.clone();
    compat.set(
        "read",
        lua.create_function(move |lua, args: MultiValue| {
            read_from_compat_input(lua, read_state.clone(), args)
        })?,
    )?;
    let write_state = compat_state.clone();
    compat.set(
        "write",
        lua.create_function(move |_, values: MultiValue| {
            write_to_compat_output(write_state.clone(), values)
        })?,
    )?;
    let flush_state = compat_state.clone();
    compat.set(
        "flush",
        lua.create_function(move |_, ()| flush_compat_output(flush_state.clone()))?,
    )?;
    let close_state = compat_state.clone();
    compat.set(
        "close",
        lua.create_function(move |_, value: LuaValue| {
            close_compat_file(close_state.clone(), value)
        })?,
    )?;
    compat.set(
        "type",
        lua.create_function(|_, value: LuaValue| match value {
            LuaValue::UserData(userdata) if userdata.is::<ManagedIoFile>() => {
                let file = userdata.borrow::<ManagedIoFile>()?;
                if file.is_closed()? {
                    Ok("closed file")
                } else {
                    Ok("file")
                }
            }
            _ => Ok("nil"),
        })?,
    )?;
    lua.globals().set("io", compat.clone())?;
    if let Ok(package) = lua.globals().get::<Table>("package") {
        if let Ok(loaded) = package.get::<Table>("loaded") {
            loaded.set("io", compat.clone())?;
        }
        if let Ok(preload) = package.get::<Table>("preload") {
            let compat_for_require = compat.clone();
            preload.set(
                "io",
                lua.create_function(move |_, ()| Ok(compat_for_require.clone()))?,
            )?;
        }
    }
    Ok(())
}

/// Set or return the current managed default input handle.
/// 设置或返回当前托管默认输入句柄。
fn set_or_get_compat_input(
    lua: &Lua,
    state: Arc<Mutex<ManagedIoCompatState>>,
    value: LuaValue,
    options: ManagedIoOptions,
) -> mlua::Result<LuaValue> {
    match value {
        LuaValue::Nil => {
            let current = lock_compat_state(&state).current_input.clone();
            managed_file_to_lua_value(lua, current)
        }
        LuaValue::String(path) => {
            let path = require_path_arg(LuaValue::String(path), "io.input", "file")?;
            let file = ManagedIoFile::open(
                PathBuf::from(path),
                ManagedIoOpenMode {
                    kind: ManagedIoModeKind::Read,
                    binary: false,
                    update: false,
                },
                options.default_encoding,
            )?;
            lock_compat_state(&state).current_input = Some(file.clone());
            Ok(LuaValue::UserData(lua.create_userdata(file)?))
        }
        LuaValue::UserData(userdata) if userdata.is::<ManagedIoFile>() => {
            let file = {
                let borrowed = userdata.borrow::<ManagedIoFile>()?;
                borrowed.clone()
            };
            lock_compat_state(&state).current_input = Some(file);
            Ok(LuaValue::UserData(userdata))
        }
        other => Err(mlua::Error::runtime(format!(
            "io.input expected path string or managed file, got {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Set or return the current managed default output handle.
/// 设置或返回当前托管默认输出句柄。
fn set_or_get_compat_output(
    lua: &Lua,
    state: Arc<Mutex<ManagedIoCompatState>>,
    value: LuaValue,
    options: ManagedIoOptions,
) -> mlua::Result<LuaValue> {
    match value {
        LuaValue::Nil => {
            let current = lock_compat_state(&state).current_output.clone();
            managed_file_to_lua_value(lua, current)
        }
        LuaValue::String(path) => {
            let path = require_path_arg(LuaValue::String(path), "io.output", "file")?;
            let file = ManagedIoFile::open(
                PathBuf::from(path),
                ManagedIoOpenMode {
                    kind: ManagedIoModeKind::Write,
                    binary: false,
                    update: false,
                },
                options.default_encoding,
            )?;
            lock_compat_state(&state).current_output = Some(file.clone());
            Ok(LuaValue::UserData(lua.create_userdata(file)?))
        }
        LuaValue::UserData(userdata) if userdata.is::<ManagedIoFile>() => {
            let file = {
                let borrowed = userdata.borrow::<ManagedIoFile>()?;
                borrowed.clone()
            };
            lock_compat_state(&state).current_output = Some(file);
            Ok(LuaValue::UserData(userdata))
        }
        other => Err(mlua::Error::runtime(format!(
            "io.output expected path string or managed file, got {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Read from the current managed default input handle.
/// 从当前托管默认输入句柄读取。
fn read_from_compat_input(
    lua: &Lua,
    state: Arc<Mutex<ManagedIoCompatState>>,
    args: MultiValue,
) -> mlua::Result<MultiValue> {
    let file = lock_compat_state(&state)
        .current_input
        .clone()
        .ok_or_else(|| {
            mlua::Error::runtime("io.read has no managed input; call io.input(path_or_file) first")
        })?;
    file.read_values(lua, args)
}

/// Write to the current managed default output handle or captured runtime log.
/// 写入当前托管默认输出句柄或捕获到运行时日志。
fn write_to_compat_output(
    state: Arc<Mutex<ManagedIoCompatState>>,
    values: MultiValue,
) -> mlua::Result<bool> {
    let file = lock_compat_state(&state).current_output.clone();
    if let Some(file) = file {
        return file.write_values(values);
    }
    let mut parts = Vec::new();
    for value in values {
        parts.push(lua_value_to_display_text(value)?);
    }
    crate::runtime_logging::info(format!("[LuaSkill:stdout] {}", parts.concat()));
    Ok(true)
}

/// Flush the current managed default output handle when one is configured.
/// 在已配置默认输出句柄时刷新它。
fn flush_compat_output(state: Arc<Mutex<ManagedIoCompatState>>) -> mlua::Result<bool> {
    let file = lock_compat_state(&state).current_output.clone();
    match file {
        Some(file) => file.flush(),
        None => Ok(true),
    }
}

/// Close an explicit managed file or the current managed default output handle.
/// 关闭显式托管文件或当前托管默认输出句柄。
fn close_compat_file(
    state: Arc<Mutex<ManagedIoCompatState>>,
    value: LuaValue,
) -> mlua::Result<bool> {
    match value {
        LuaValue::Nil => {
            let file = lock_compat_state(&state).current_output.take();
            match file {
                Some(file) => file.close(),
                None => Ok(true),
            }
        }
        LuaValue::UserData(userdata) if userdata.is::<ManagedIoFile>() => {
            let file = userdata.borrow::<ManagedIoFile>()?;
            file.close()
        }
        other => Err(mlua::Error::runtime(format!(
            "io.close expected managed file, got {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Lock the managed IO compatibility state and return its guard, recovering after state lock poisoning.
/// 锁定并返回托管 IO 兼容状态保护对象；如果状态锁已 poison，则恢复继续使用。
fn lock_compat_state(state: &Mutex<ManagedIoCompatState>) -> MutexGuard<'_, ManagedIoCompatState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Convert an optional managed file into a Lua userdata value.
/// 将可选托管文件转换为 Lua userdata 值。
fn managed_file_to_lua_value(lua: &Lua, file: Option<ManagedIoFile>) -> mlua::Result<LuaValue> {
    match file {
        Some(file) => Ok(LuaValue::UserData(lua.create_userdata(file)?)),
        None => Ok(LuaValue::Nil),
    }
}

/// Open one managed temporary file from Lua arguments.
/// 从 Lua 参数打开一个托管临时文件。
fn tmpfile_from_args(lua: &Lua, options: ManagedIoOptions) -> mlua::Result<LuaValue> {
    let file = ManagedIoFile::tmpfile(options.default_encoding)?;
    Ok(LuaValue::UserData(lua.create_userdata(file)?))
}

/// Open one managed file from Lua argument values.
/// 从 Lua 参数值打开一个托管文件。
fn open_from_args(
    lua: &Lua,
    args: MultiValue,
    io_options: ManagedIoOptions,
) -> mlua::Result<LuaValue> {
    let mut values = args.into_iter();
    let path = require_path_arg(
        values.next().unwrap_or(LuaValue::Nil),
        "vulcan.io.open",
        "path",
    )?;
    let mode_text = match values.next().unwrap_or(LuaValue::Nil) {
        LuaValue::Nil => None,
        value => Some(require_string_arg(value, "vulcan.io.open", "mode", false)?),
    };
    let options = values.next().unwrap_or(LuaValue::Nil);
    let open_mode = parse_open_mode(mode_text.as_deref().unwrap_or("r"))?;
    let encoding = parse_encoding_options(options, "vulcan.io.open", io_options.default_encoding)?;
    let file = ManagedIoFile::open(PathBuf::from(path), open_mode, encoding)?;
    Ok(LuaValue::UserData(lua.create_userdata(file)?))
}

/// Read a whole text file through `vulcan.io.read_text`.
/// 通过 `vulcan.io.read_text` 读取完整文本文件。
fn read_text_from_args(
    lua: &Lua,
    args: MultiValue,
    io_options: ManagedIoOptions,
) -> mlua::Result<LuaValue> {
    let mut values = args.into_iter();
    let path = require_path_arg(
        values.next().unwrap_or(LuaValue::Nil),
        "vulcan.io.read_text",
        "path",
    )?;
    let options = values.next().unwrap_or(LuaValue::Nil);
    let encoding =
        parse_encoding_options(options, "vulcan.io.read_text", io_options.default_encoding)?;
    let bytes =
        fs::read(path).map_err(|error| mlua::Error::runtime(format!("read_text: {error}")))?;
    bytes_to_lua_value(lua, &bytes, false, encoding)
}

/// Write or append a whole text file through `vulcan.io.write_text`.
/// 通过 `vulcan.io.write_text` 写入或追加完整文本文件。
fn write_text_from_args(
    args: MultiValue,
    append: bool,
    io_options: ManagedIoOptions,
) -> mlua::Result<bool> {
    let mut values = args.into_iter();
    let fn_name = if append {
        "vulcan.io.append_text"
    } else {
        "vulcan.io.write_text"
    };
    let path = require_path_arg(values.next().unwrap_or(LuaValue::Nil), fn_name, "path")?;
    let content = require_string_arg(
        values.next().unwrap_or(LuaValue::Nil),
        fn_name,
        "content",
        true,
    )?;
    let options = values.next().unwrap_or(LuaValue::Nil);
    let encoding = parse_encoding_options(options, fn_name, io_options.default_encoding)?;
    let bytes = encode_runtime_text(&content, encoding)
        .map_err(|error| mlua::Error::runtime(format!("{fn_name}: {error}")))?;
    if append {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| std::io::Write::write_all(&mut file, &bytes))
            .map_err(|error| mlua::Error::runtime(format!("{fn_name}: {error}")))?;
    } else {
        fs::write(&path, bytes)
            .map_err(|error| mlua::Error::runtime(format!("{fn_name}: {error}")))?;
    }
    Ok(true)
}

/// Create a line iterator from `io.lines` or `vulcan.io.lines` arguments.
/// 根据 `io.lines` 或 `vulcan.io.lines` 参数创建行迭代器。
fn lines_from_args(
    lua: &Lua,
    args: MultiValue,
    io_options: ManagedIoOptions,
) -> mlua::Result<Function> {
    let mut values = args.into_iter();
    let path = require_path_arg(
        values.next().unwrap_or(LuaValue::Nil),
        "vulcan.io.lines",
        "path",
    )?;
    let options = values.next().unwrap_or(LuaValue::Nil);
    let encoding = parse_encoding_options(options, "vulcan.io.lines", io_options.default_encoding)?;
    let file = ManagedIoFile::open(
        PathBuf::from(path),
        ManagedIoOpenMode {
            kind: ManagedIoModeKind::Read,
            binary: false,
            update: false,
        },
        encoding,
    )?;
    file.lines(lua)
}

/// Popen execution options for one managed read command.
/// 单次托管读取命令的 popen 执行选项。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ManagedPopenOptions {
    /// Text encoding used when Lua reads the captured command output.
    /// Lua 读取捕获命令输出时使用的文本编码。
    encoding: RuntimeTextEncoding,
    /// Maximum time allowed for the spawned command.
    /// 允许启动命令运行的最大时长。
    timeout_ms: u64,
}

/// Captured output from one managed popen command.
/// 单次托管 popen 命令捕获到的输出。
struct ManagedPopenOutput {
    /// Standard output bytes exposed through the returned file-like handle.
    /// 通过返回的类文件句柄暴露的标准输出字节。
    stdout: Vec<u8>,
    /// Whether the spawned command exited successfully.
    /// 启动的命令是否成功退出。
    success: bool,
}

/// Open one Rust-managed popen read handle from Lua arguments.
/// 根据 Lua 参数打开一个 Rust 托管的 popen 读取句柄。
fn popen_from_args(
    lua: &Lua,
    args: MultiValue,
    io_options: ManagedIoOptions,
) -> mlua::Result<LuaValue> {
    let mut values = args.into_iter();
    let command = require_string_arg(
        values.next().unwrap_or(LuaValue::Nil),
        "vulcan.io.popen",
        "command",
        false,
    )?;
    let second = values.next().unwrap_or(LuaValue::Nil);
    let (mode_text, options_value) = match second {
        LuaValue::Nil => (None, values.next().unwrap_or(LuaValue::Nil)),
        LuaValue::String(_) => (
            Some(require_string_arg(
                second,
                "vulcan.io.popen",
                "mode",
                false,
            )?),
            values.next().unwrap_or(LuaValue::Nil),
        ),
        LuaValue::Table(_) => (None, second),
        other => {
            return Err(mlua::Error::runtime(format!(
                "vulcan.io.popen: mode must be a string or options table, got {}",
                lua_value_type_name(&other)
            )));
        }
    };
    let mode = parse_popen_mode(mode_text.as_deref().unwrap_or("r"))?;
    let options = parse_popen_options(
        options_value,
        "vulcan.io.popen",
        io_options.default_encoding,
    )?;
    let output = run_managed_popen_read(&command, options)?;
    let file = ManagedIoFile::from_read_buffer(
        format!("<popen:{command}>"),
        mode,
        options.encoding,
        output.stdout,
        Some(ManagedIoCloseStatus {
            success: output.success,
        }),
    );
    Ok(LuaValue::UserData(lua.create_userdata(file)?))
}

/// Parse a Lua popen mode and reject unsupported write modes explicitly.
/// 解析 Lua popen 模式，并明确拒绝暂不支持的写入模式。
fn parse_popen_mode(mode: &str) -> mlua::Result<ManagedIoOpenMode> {
    let binary = mode.contains('b');
    let normalized = mode.replace('b', "");
    match normalized.as_str() {
        "r" | "" => Ok(ManagedIoOpenMode {
            kind: ManagedIoModeKind::Read,
            binary,
            update: false,
        }),
        "w" => Err(mlua::Error::runtime(
            "vulcan.io.popen: write mode is not implemented yet",
        )),
        _ => Err(mlua::Error::runtime(format!(
            "vulcan.io.popen: unsupported mode `{mode}`"
        ))),
    }
}

/// Parse optional popen encoding and timeout options.
/// 解析可选的 popen 编码与超时选项。
fn parse_popen_options(
    value: LuaValue,
    fn_name: &str,
    default_encoding: RuntimeTextEncoding,
) -> mlua::Result<ManagedPopenOptions> {
    let default_timeout_ms = 60_000_u64;
    match value {
        LuaValue::Nil => Ok(ManagedPopenOptions {
            encoding: default_encoding,
            timeout_ms: default_timeout_ms,
        }),
        LuaValue::String(_) => Ok(ManagedPopenOptions {
            encoding: parse_encoding_options(value, fn_name, default_encoding)?,
            timeout_ms: default_timeout_ms,
        }),
        LuaValue::Table(table) => {
            let encoding_value: LuaValue = table.get("encoding")?;
            let timeout_value: LuaValue = table.get("timeout_ms")?;
            Ok(ManagedPopenOptions {
                encoding: parse_encoding_options(encoding_value, fn_name, default_encoding)?,
                timeout_ms: parse_timeout_ms_option(timeout_value, fn_name, default_timeout_ms)?,
            })
        }
        other => Err(mlua::Error::runtime(format!(
            "{fn_name}: options must be nil, string, or table, got {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Parse a positive timeout value from a Lua option.
/// 从 Lua 选项中解析正数超时时长。
fn parse_timeout_ms_option(
    value: LuaValue,
    fn_name: &str,
    default_timeout_ms: u64,
) -> mlua::Result<u64> {
    match value {
        LuaValue::Nil => Ok(default_timeout_ms),
        LuaValue::Integer(number) if number > 0 => Ok(number as u64),
        LuaValue::Number(number) if number.is_finite() && number > 0.0 => Ok(number as u64),
        other => Err(mlua::Error::runtime(format!(
            "{fn_name}: timeout_ms must be a positive number, got {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Run one shell command for managed popen read mode and capture output bytes.
/// 为托管 popen 读取模式运行一个 shell 命令并捕获输出字节。
fn run_managed_popen_read(
    command_text: &str,
    options: ManagedPopenOptions,
) -> mlua::Result<ManagedPopenOutput> {
    let mut command = create_shell_command(command_text);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Managed one-shot ownership tuple that prevents timeout and direct-exit descendants from escaping.
    // 防止超时或直接子进程退出后的后代逃逸的受管单次执行所有权元组。
    let (mut child, process_tree, reaper_permit) =
        ManagedChildProcessTree::spawn_with_keepalive(&mut command, "vulcan.io.popen", None)
            .map_err(|error| mlua::Error::runtime(format!("vulcan.io.popen: {error}")))?;
    let stdout_handle = child.stdout.take().map(spawn_popen_pipe_reader);
    let deadline = Instant::now() + Duration::from_millis(options.timeout_ms);
    let mut timed_out = false;
    // Direct-child wait failure retained until unified process-tree cleanup has preserved ownership.
    // 保留到统一进程树清理完成所有权保护后的直接子进程等待失败。
    let mut wait_error = None;

    // Direct-child status observed before remaining descendants are terminated and reaped.
    // 终止并回收残留后代前观察到的直接子进程状态。
    let observed_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                break None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                wait_error = Some(format!("vulcan.io.popen wait: {error}"));
                break None;
            }
        }
    };

    // Definitive status available only after the entire one-shot process tree has converged.
    // 仅在整棵单次执行进程树收敛后可用的确定终态。
    let status = match finalize_one_shot_process_tree(
        child,
        process_tree,
        reaper_permit,
        observed_status,
        "vulcan.io.popen",
    ) {
        Ok(status) if wait_error.is_none() => status,
        Ok(_) => {
            return Err(mlua::Error::runtime(wait_error.unwrap_or_else(|| {
                "vulcan.io.popen failed to wait for process".to_string()
            })));
        }
        Err(cleanup_error) => {
            // Combined error retains the primary wait failure next to the cleanup failure.
            // 组合错误会把主要等待失败与清理失败并列保留。
            let error = match wait_error {
                Some(wait_error) => format!("{wait_error}; cleanup failed: {cleanup_error}"),
                None => cleanup_error,
            };
            return Err(mlua::Error::runtime(error));
        }
    };

    let stdout = join_popen_pipe_reader(stdout_handle, "stdout")?;
    if timed_out {
        return Err(mlua::Error::runtime(format!(
            "vulcan.io.popen timed out after {} ms",
            options.timeout_ms
        )));
    }

    Ok(ManagedPopenOutput {
        stdout,
        success: status.success(),
    })
}

/// Create the platform shell command used by managed popen.
/// 创建托管 popen 使用的平台 shell 命令。
fn create_shell_command(command_text: &str) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("cmd");
        command.arg("/C").arg(command_text);
        command
    }

    #[cfg(not(windows))]
    {
        let mut command = Command::new("sh");
        command.arg("-c").arg(command_text);
        command
    }
}

/// Spawn one background reader that drains a process pipe into bytes.
/// 启动一个后台读取器，将进程管道排空为字节。
fn spawn_popen_pipe_reader<R>(mut reader: R) -> thread::JoinHandle<std::io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = Vec::new();
        reader.read_to_end(&mut buffer)?;
        Ok(buffer)
    })
}

/// Join one popen pipe reader and convert failures into Lua errors.
/// 等待一个 popen 管道读取器，并将失败转换为 Lua 错误。
fn join_popen_pipe_reader(
    handle: Option<thread::JoinHandle<std::io::Result<Vec<u8>>>>,
    stream_name: &str,
) -> mlua::Result<Vec<u8>> {
    match handle {
        Some(handle) => handle
            .join()
            .map_err(|_| {
                mlua::Error::runtime(format!("vulcan.io.popen {stream_name} reader panicked"))
            })?
            .map_err(|error| {
                mlua::Error::runtime(format!("vulcan.io.popen {stream_name}: {error}"))
            }),
        None => Ok(Vec::new()),
    }
}

/// Reserve one unique temporary file path for `io.tmpfile`.
/// 为 `io.tmpfile` 预留一个唯一临时文件路径。
fn reserve_tmpfile_path() -> mlua::Result<PathBuf> {
    let temp_dir = std::env::temp_dir();
    let epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    for _ in 0..128 {
        let sequence = TMPFILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = temp_dir.join(format!(
            "luaskills_managed_tmpfile_{}_{}_{}.tmp",
            std::process::id(),
            epoch_ms,
            sequence
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(mlua::Error::runtime(format!(
                    "io.tmpfile: failed to reserve temp file: {error}"
                )));
            }
        }
    }
    Err(mlua::Error::runtime(
        "io.tmpfile: failed to reserve a unique temp file name",
    ))
}

/// Parse one Lua open mode string into a managed mode.
/// 将一个 Lua 打开模式字符串解析为托管模式。
fn parse_open_mode(mode: &str) -> mlua::Result<ManagedIoOpenMode> {
    let binary = mode.contains('b');
    let update = mode.contains('+');
    let normalized = mode.replace(['b', '+'], "");
    let kind = match normalized.as_str() {
        "r" | "" => ManagedIoModeKind::Read,
        "w" => ManagedIoModeKind::Write,
        "a" => ManagedIoModeKind::Append,
        _ => {
            return Err(mlua::Error::runtime(format!(
                "vulcan.io.open: unsupported mode `{mode}`"
            )));
        }
    };
    Ok(ManagedIoOpenMode {
        kind,
        binary,
        update,
    })
}

/// Parse optional encoding configuration from a Lua options value.
/// 从 Lua 选项值中解析可选编码配置。
fn parse_encoding_options(
    value: LuaValue,
    fn_name: &str,
    default_encoding: RuntimeTextEncoding,
) -> mlua::Result<RuntimeTextEncoding> {
    match value {
        LuaValue::Nil => Ok(default_encoding),
        LuaValue::String(label) => {
            let label = label
                .to_str()
                .map_err(|_| mlua::Error::runtime(format!("{fn_name}: encoding must be UTF-8")))?;
            RuntimeTextEncoding::parse(label.as_ref())
                .map_err(|error| mlua::Error::runtime(format!("{fn_name}: {error}")))
        }
        LuaValue::Table(table) => {
            let encoding_value: LuaValue = table.get("encoding")?;
            parse_encoding_options(encoding_value, fn_name, default_encoding)
        }
        other => Err(mlua::Error::runtime(format!(
            "{fn_name}: options must be nil, string, or table, got {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Convert raw bytes to one Lua value according to binary/text mode.
/// 按二进制或文本模式将原始字节转换为一个 Lua 值。
fn bytes_to_lua_value(
    lua: &Lua,
    bytes: &[u8],
    binary: bool,
    encoding: RuntimeTextEncoding,
) -> mlua::Result<LuaValue> {
    if binary {
        return Ok(LuaValue::String(lua.create_string(bytes)?));
    }
    let decoded = decode_runtime_text(bytes, encoding);
    Ok(LuaValue::String(lua.create_string(&decoded.text)?))
}

/// Convert one Lua value into output bytes for file writes.
/// 将一个 Lua 值转换为文件写入用的输出字节。
fn lua_value_to_output_bytes(
    value: LuaValue,
    binary: bool,
    encoding: RuntimeTextEncoding,
) -> mlua::Result<Vec<u8>> {
    match value {
        LuaValue::String(text) if binary => Ok(text.as_bytes().to_vec()),
        LuaValue::String(text) => {
            let text = text.to_str().map_err(|_| {
                mlua::Error::runtime("file:write string must be valid UTF-8 in text mode")
            })?;
            encode_runtime_text(text.as_ref(), encoding)
                .map_err(|error| mlua::Error::runtime(format!("file:write: {error}")))
        }
        LuaValue::Integer(number) => encode_runtime_text(&number.to_string(), encoding)
            .map_err(|error| mlua::Error::runtime(format!("file:write: {error}"))),
        LuaValue::Number(number) => encode_runtime_text(&number.to_string(), encoding)
            .map_err(|error| mlua::Error::runtime(format!("file:write: {error}"))),
        LuaValue::Boolean(flag) => encode_runtime_text(&flag.to_string(), encoding)
            .map_err(|error| mlua::Error::runtime(format!("file:write: {error}"))),
        other => Err(mlua::Error::runtime(format!(
            "file:write unsupported value {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Convert one Lua value into strict UTF-8 stdout text for managed `io.write`.
/// 将单个 Lua 值转换为托管 `io.write` 使用的严格 UTF-8 stdout 文本。
fn lua_value_to_display_text(value: LuaValue) -> mlua::Result<String> {
    match value {
        LuaValue::String(text) => {
            // Stdout logging is textual, so invalid Lua byte strings must fail instead of being replaced.
            // stdout 日志是文本语义，因此无效 Lua 字节字符串必须报错而不是被替换。
            let text = text.to_str().map_err(|_| {
                mlua::Error::runtime(
                    "io.write string must be valid UTF-8 when no output file is selected",
                )
            })?;
            Ok(text.as_ref().to_string())
        }
        LuaValue::Integer(number) => Ok(number.to_string()),
        LuaValue::Number(number) => Ok(number.to_string()),
        LuaValue::Boolean(flag) => Ok(flag.to_string()),
        LuaValue::Nil => Ok("nil".to_string()),
        other => Ok(format!("{other:?}")),
    }
}

/// Flush one managed file state according to its write mode.
/// 按写入模式刷新一个托管文件状态。
fn flush_state(state: &mut ManagedIoFileState) -> mlua::Result<()> {
    if state.mode.update {
        return flush_update_state(state);
    }
    match state.mode.kind {
        ManagedIoModeKind::Read => Ok(()),
        ManagedIoModeKind::Write => flush_write_state(state),
        ManagedIoModeKind::Append => {
            // Pending append suffix owned by this handle since the last successful flush.
            // 当前句柄自上次成功刷新后拥有的待追加后缀。
            let pending = &state.buffer[state.flushed_len..];
            // Pending byte count retained after the immutable slice borrow ends.
            // 在不可变切片借用结束后保留的待写字节数。
            #[cfg(test)]
            let pending_len = pending.len();
            if !pending.is_empty() {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&state.path)
                    .and_then(|mut file| file.write_all(pending))
                    .map_err(|error| mlua::Error::runtime(format!("file:flush: {error}")))?;
                state.flushed_len = state.buffer.len();
                #[cfg(test)]
                {
                    state.physical_write_bytes =
                        state.physical_write_bytes.saturating_add(pending_len);
                }
            }
            Ok(())
        }
    }
}

/// Publish the complete authoritative buffer and reset incremental flush bookkeeping.
/// 发布完整权威缓冲区并重置增量刷新记账。
fn flush_full_buffer(state: &mut ManagedIoFileState) -> mlua::Result<()> {
    // Full payload size recorded only after the filesystem write succeeds.
    // 仅在文件系统写入成功后记录的完整载荷大小。
    let payload_len = state.buffer.len();
    // File remains open through publication and generation capture so both describe one exact object.
    // File 在发布与变更代捕获期间保持打开，确保二者描述同一个精确对象。
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&state.path)
        .and_then(|mut file| {
            file.write_all(&state.buffer)?;
            file.flush()?;
            Ok(file)
        })
        .map_err(|error| mlua::Error::runtime(format!("file:flush: {error}")))?;
    // FlushedFingerprint is captured only after the complete publication succeeds.
    // FlushedFingerprint 仅在完整发布成功后获取。
    let flushed_fingerprint = ManagedIoBackingFingerprint::from_file(&file)
        .map_err(|error| mlua::Error::runtime(format!("file:flush: {error}")))?;
    state.flushed_len = payload_len;
    state.flush_initialized = true;
    state.flushed_fingerprint = Some(flushed_fingerprint);
    state.dirty_range = None;
    #[cfg(test)]
    {
        state.physical_write_bytes = state.physical_write_bytes.saturating_add(payload_len);
    }
    Ok(())
}

/// Flush non-update write mode by truncating once and then writing only the new suffix.
/// 刷新非更新写入模式：首次截断，随后只写入新增后缀。
fn flush_write_state(state: &mut ManagedIoFileState) -> mlua::Result<()> {
    if !state.flush_initialized || state.flushed_len > state.buffer.len() {
        return flush_full_buffer(state);
    }
    if !managed_io_backing_file_matches_last_flush(state)? {
        return flush_full_buffer(state);
    }
    // First byte not represented by the last successful flush.
    // 上次成功刷新尚未表示的第一个字节位置。
    let pending_start = state.flushed_len;
    if pending_start == state.buffer.len() {
        return Ok(());
    }
    // Pending suffix byte count used for test-only physical-write accounting.
    // 用于测试专属物理写入计数的待写后缀字节数。
    #[cfg(test)]
    let pending_len = state.buffer.len() - pending_start;
    // Existing output file used for positioned suffix publication.
    // 用于定位发布后缀的已有输出文件。
    let mut file = match OpenOptions::new().write(true).open(&state.path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return flush_full_buffer(state),
        Err(error) => return Err(mlua::Error::runtime(format!("file:flush: {error}"))),
    };
    // Persisted offset expressed in the filesystem seek type.
    // 以文件系统 seek 类型表示的已落盘偏移。
    let pending_offset = u64::try_from(pending_start)
        .map_err(|error| mlua::Error::runtime(format!("file:flush: {error}")))?;
    // Final authoritative length applied after the suffix write.
    // 在后缀写入后应用的最终权威长度。
    let final_len = u64::try_from(state.buffer.len())
        .map_err(|error| mlua::Error::runtime(format!("file:flush: {error}")))?;
    file.seek(SeekFrom::Start(pending_offset))
        .and_then(|_| file.write_all(&state.buffer[pending_start..]))
        .and_then(|_| file.set_len(final_len))
        .and_then(|_| file.flush())
        .map_err(|error| mlua::Error::runtime(format!("file:flush: {error}")))?;
    // FlushedFingerprint describes the exact file handle after suffix publication.
    // FlushedFingerprint 描述后缀发布后的精确文件句柄。
    let flushed_fingerprint = ManagedIoBackingFingerprint::from_file(&file)
        .map_err(|error| mlua::Error::runtime(format!("file:flush: {error}")))?;
    state.flushed_len = state.buffer.len();
    state.flushed_fingerprint = Some(flushed_fingerprint);
    #[cfg(test)]
    {
        state.physical_write_bytes = state.physical_write_bytes.saturating_add(pending_len);
    }
    Ok(())
}

/// Flush the union of update-mode dirty bytes while retaining the in-memory buffer as truth.
/// 刷新更新模式脏字节并继续以内存缓冲区作为事实源。
fn flush_update_state(state: &mut ManagedIoFileState) -> mlua::Result<()> {
    if !state.flush_initialized {
        return flush_full_buffer(state);
    }
    if !managed_io_backing_file_matches_last_flush(state)? {
        return flush_full_buffer(state);
    }
    // Dirty range accumulated from every successful logical write since the last flush.
    // 自上次刷新后每次成功逻辑写入累计得到的脏范围。
    let Some((dirty_start, dirty_end)) = state.dirty_range else {
        return Ok(());
    };
    // Dirty bytes validated against the authoritative buffer before any filesystem mutation.
    // 在任何文件系统变更前相对权威缓冲区验证的脏字节。
    let dirty_bytes = state.buffer.get(dirty_start..dirty_end).ok_or_else(|| {
        mlua::Error::runtime("file:flush: managed update dirty range is outside the buffer")
    })?;
    // Dirty payload length retained after its immutable buffer borrow ends.
    // 在不可变缓冲区借用结束后保留的脏载荷长度。
    #[cfg(test)]
    let dirty_len = dirty_bytes.len();
    // Existing update target opened without implicit truncation.
    // 在不隐式截断情况下打开的已有更新目标。
    let mut file = match OpenOptions::new().write(true).open(&state.path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return flush_full_buffer(state),
        Err(error) => return Err(mlua::Error::runtime(format!("file:flush: {error}"))),
    };
    // Dirty offset expressed in the filesystem seek type.
    // 以文件系统 seek 类型表示的脏偏移。
    let dirty_offset = u64::try_from(dirty_start)
        .map_err(|error| mlua::Error::runtime(format!("file:flush: {error}")))?;
    // Final authoritative length used for update-mode extension or truncation.
    // 用于更新模式扩展或截断的最终权威长度。
    let final_len = u64::try_from(state.buffer.len())
        .map_err(|error| mlua::Error::runtime(format!("file:flush: {error}")))?;
    file.seek(SeekFrom::Start(dirty_offset))
        .and_then(|_| file.write_all(dirty_bytes))
        .and_then(|_| file.set_len(final_len))
        .and_then(|_| file.flush())
        .map_err(|error| mlua::Error::runtime(format!("file:flush: {error}")))?;
    // FlushedFingerprint describes the exact file handle after dirty-range publication.
    // FlushedFingerprint 描述脏区发布后的精确文件句柄。
    let flushed_fingerprint = ManagedIoBackingFingerprint::from_file(&file)
        .map_err(|error| mlua::Error::runtime(format!("file:flush: {error}")))?;
    state.flushed_len = state.buffer.len();
    state.flushed_fingerprint = Some(flushed_fingerprint);
    state.dirty_range = None;
    #[cfg(test)]
    {
        state.physical_write_bytes = state.physical_write_bytes.saturating_add(dirty_len);
    }
    Ok(())
}

/// Verify that the current backing file still matches the observable generation published by the last successful flush.
/// 验证当前底层文件仍与上次成功刷新所发布的可观察代一致。
///
/// The state parameter owns the expected metadata generation and backing path for one writable handle.
/// state 参数持有单个可写句柄的预期元数据代与底层路径。
///
/// Returns true only when length and an available timestamp prove the backing generation is unchanged, false for a missing, changed, or unverifiable generation, and an error for inaccessible metadata.
/// 仅当长度与可用时间戳证明底层代未变化时返回 true；文件缺失、已变化或无法验证时返回 false，元数据不可访问时返回错误。
fn managed_io_backing_file_matches_last_flush(state: &ManagedIoFileState) -> mlua::Result<bool> {
    // ExpectedFingerprint is absent before the first successful full publication.
    // ExpectedFingerprint 在首次完整发布成功前不存在。
    let Some(expected_fingerprint) = state.flushed_fingerprint.as_ref() else {
        return Ok(false);
    };
    // File is opened read-only so validation cannot mutate the current generation.
    // File 以只读方式打开，确保验证不会修改当前代。
    let file = match File::open(&state.path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(mlua::Error::runtime(format!("file:flush: {error}"))),
    };
    // ActualFingerprint is compared in constant time; missing platform generations force safe full publication.
    // ActualFingerprint 以常量时间比较；缺失平台变更代时强制执行安全的完整发布。
    let actual_fingerprint = ManagedIoBackingFingerprint::from_file(&file)
        .map_err(|error| mlua::Error::runtime(format!("file:flush: {error}")))?;
    let has_platform_generation = actual_fingerprint.platform_generation.is_some()
        && expected_fingerprint.platform_generation.is_some();
    Ok(has_platform_generation && &actual_fingerprint == expected_fingerprint)
}

/// Ensure one managed file handle is still open.
/// 确保一个托管文件句柄仍处于打开状态。
fn ensure_file_is_open(state: &ManagedIoFileState, operation_name: &str) -> mlua::Result<()> {
    if state.closed {
        return Err(mlua::Error::runtime(format!(
            "{operation_name}: file is already closed"
        )));
    }
    Ok(())
}

/// Ensure one managed file handle can be read.
/// 确保一个托管文件句柄可以读取。
fn ensure_file_is_readable(state: &ManagedIoFileState, operation_name: &str) -> mlua::Result<()> {
    if state.mode.kind != ManagedIoModeKind::Read && !state.mode.update {
        return Err(mlua::Error::runtime(format!(
            "{operation_name}: file is not opened for reading"
        )));
    }
    Ok(())
}

/// Ensure one managed file handle can be written.
/// 确保一个托管文件句柄可以写入。
fn ensure_file_is_writable(state: &ManagedIoFileState, operation_name: &str) -> mlua::Result<()> {
    if matches!(state.mode.kind, ManagedIoModeKind::Read) && !state.mode.update {
        return Err(mlua::Error::runtime(format!(
            "{operation_name}: file is not opened for writing"
        )));
    }
    Ok(())
}

/// Require one strict UTF-8 Lua string argument.
/// 要求一个严格 UTF-8 Lua 字符串参数。
fn require_string_arg(
    value: LuaValue,
    fn_name: &str,
    param_name: &str,
    allow_blank: bool,
) -> mlua::Result<String> {
    let text = match value {
        LuaValue::String(text) => text
            .to_str()
            .map_err(|_| {
                mlua::Error::runtime(format!("{fn_name}: {param_name} must be valid UTF-8"))
            })?
            .to_string(),
        other => {
            return Err(mlua::Error::runtime(format!(
                "{fn_name}: {param_name} must be a string, got {}",
                lua_value_type_name(&other)
            )));
        }
    };
    if !allow_blank && text.trim().is_empty() {
        return Err(mlua::Error::runtime(format!(
            "{fn_name}: {param_name} must not be empty"
        )));
    }
    if text.contains('\0') {
        return Err(mlua::Error::runtime(format!(
            "{fn_name}: {param_name} must not contain NUL bytes"
        )));
    }
    Ok(text)
}

/// Require one path argument with basic syntax validation.
/// 要求一个带基础语法校验的路径参数。
fn require_path_arg(value: LuaValue, fn_name: &str, param_name: &str) -> mlua::Result<String> {
    let path = require_string_arg(value, fn_name, param_name, false)?;
    if looks_like_lua_debug_value(&path) {
        return Err(mlua::Error::runtime(format!(
            "{fn_name}: {param_name} looks like a coerced Lua object string `{path}`"
        )));
    }
    // Host-visible drive/UNC spelling normalized and unsupported namespaces rejected before lookup.
    // 在寻址前归一化宿主可见盘符/UNC 写法，并拒绝不受支持的命名空间。
    let path = normalize_host_input_path_text(&path)
        .map_err(|error| mlua::Error::runtime(format!("{fn_name}: {param_name}: {error}")))?;
    #[cfg(windows)]
    if has_invalid_windows_path_syntax(&path) {
        return Err(mlua::Error::runtime(format!(
            "{fn_name}: {param_name} contains invalid Windows path syntax"
        )));
    }
    Ok(path)
}

/// Detect Lua debug-style object strings that should never be accepted as paths.
/// 检测不应被当作路径接受的 Lua 调试风格对象字符串。
fn looks_like_lua_debug_value(text: &str) -> bool {
    ["table: 0x", "function: 0x", "thread: 0x", "userdata: 0x"]
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

/// Validate Windows path syntax before filesystem access.
/// 访问文件系统前校验 Windows 路径语法。
#[cfg(windows)]
fn has_invalid_windows_path_syntax(text: &str) -> bool {
    let trimmed = text.trim();
    let first_char = trimmed.chars().next();
    for (index, ch) in trimmed.char_indices() {
        if ch.is_control() {
            return true;
        }
        if matches!(ch, '<' | '>' | '"' | '|' | '?' | '*') {
            return true;
        }
        if ch == ':' {
            let is_drive_prefix =
                index == 1 && first_char.map(|c| c.is_ascii_alphabetic()).unwrap_or(false);
            if !is_drive_prefix {
                return true;
            }
        }
    }
    false
}

/// Return a compact Lua value type name for diagnostics.
/// 返回用于诊断的紧凑 Lua 值类型名。
fn lua_value_type_name(value: &LuaValue) -> &'static str {
    match value {
        LuaValue::Nil => "nil",
        LuaValue::Boolean(_) => "boolean",
        LuaValue::LightUserData(_) => "lightuserdata",
        LuaValue::Integer(_) | LuaValue::Number(_) => "number",
        LuaValue::String(_) => "string",
        LuaValue::Table(_) => "table",
        LuaValue::Function(_) => "function",
        LuaValue::Thread(_) => "thread",
        LuaValue::UserData(_) => "userdata",
        LuaValue::Error(_) => "error",
        LuaValue::Other(_) => "other",
    }
}

#[cfg(test)]
mod tests;
