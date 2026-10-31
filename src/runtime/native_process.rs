//! Desktop-mode Lua standard-library process bridges, with native CRT byte and number IO semantics.
//! 桌面模式 Lua 标准库进程桥接，保留原生 CRT 字节与数字 IO 语义。

use std::ffi::CStr;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::ptr::NonNull;
use std::sync::{Mutex, MutexGuard};

use mlua::{
    AnyUserData, Function, IntoLuaMulti, Lua, MultiValue, Table, UserData, UserDataMethods, Value,
    Variadic,
};

use crate::LuaRuntimeHostMode;
use crate::runtime::engine::vm_closing::{HostFunctions, HostMethods};
use crate::runtime::process_session::{
    DetachedChildReaperPermit, ManagedChildProcessTree, finalize_one_shot_process_tree,
};

unsafe extern "C" {
    /// Return the current thread's CRT error slot; the caller must read it before another CRT operation.
    /// 返回当前线程的 CRT 错误槽；调用方必须在另一项 CRT 操作前读取它。
    fn _errno() -> *mut libc::c_int;

    /// Reset EOF/error indicators on the caller-owned live CRT stream; returns no value.
    /// 重置调用方拥有的存活 CRT 流的文件结束及错误标记；无返回值。
    fn clearerr(stream: *mut libc::FILE);
}

/// Owned CRT pipe stream; never shares LuaJIT's private file userdata layout.
/// 自有 CRT 管道流；不共享 LuaJIT 的私有文件 userdata 布局。
struct CrtPipe(NonNull<libc::FILE>);

// SAFETY: Every stream operation is serialized by NativePipe.state; ownership moves do not access FILE.
// 安全性：所有流操作都由 NativePipe.state 串行化；所有权移动不访问 FILE。
unsafe impl Send for CrtPipe {}

impl CrtPipe {
    /// Transfer one owned Windows pipe into a CRT stream using the explicit read/write and binary mode.
    /// 按显式读写及二进制模式，将自有 Windows 管道转移到 CRT 流。
    /// `handle` transfers ownership; `write` and `binary` choose access; returns a stream or captured CRT error.
    /// `handle` 转移所有权；`write` 和 `binary` 选择访问方式；返回流或已捕获 CRT 错误。
    fn from_handle(handle: OwnedHandle, write: bool, binary: bool) -> Result<Self, (String, i32)> {
        // Flags preserve CRT text translation and prohibit using the opposite pipe direction.
        // 标志保留 CRT 文本转换，并禁止使用相反管道方向。
        let flags = if write {
            libc::O_WRONLY
        } else {
            libc::O_RDONLY
        } | if binary { libc::O_BINARY } else { libc::O_TEXT };
        // SAFETY: The owned handle is valid; the CRT owns it only after a successful descriptor creation.
        // 安全性：自有句柄有效；仅在描述符创建成功后 CRT 才拥有它。
        let descriptor = unsafe { libc::open_osfhandle(handle.as_raw_handle() as isize, flags) };
        if descriptor == -1 {
            return Err(crt_error());
        }
        std::mem::forget(handle);
        // Mode agrees with the descriptor flags and remains a static NUL-terminated string.
        // 模式与描述符标志一致，并保持为静态零结尾字符串。
        let mode = match (write, binary) {
            (false, false) => c"rt",
            (false, true) => c"rb",
            (true, false) => c"wt",
            (true, true) => c"wb",
        };
        // SAFETY: The descriptor is exclusively owned and the mode is valid for its access flags.
        // 安全性：描述符被独占拥有，模式与其访问标志相符。
        let stream = unsafe { libc::fdopen(descriptor, mode.as_ptr()) };
        match NonNull::new(stream) {
            Some(stream) => Ok(Self(stream)),
            None => {
                // Preserve errno before closing the descriptor that failed to become a stream.
                // 关闭未能转换为流的描述符前保留 errno。
                let error = crt_error();
                // SAFETY: fdopen failed, so the descriptor still belongs to this function.
                // 安全性：fdopen 失败，描述符仍属于本函数。
                unsafe { libc::close(descriptor) };
                Err(error)
            }
        }
    }

    /// Close the owned stream once; returns its flush/close failure with the original CRT errno.
    /// 关闭自有流一次；返回刷新或关闭失败以及原始 CRT errno。
    fn close(self) -> Result<(), (String, i32)> {
        // SAFETY: This consumes the sole stream owner; forgetting it prevents a second fclose.
        // 安全性：本操作消费唯一流所有者；遗忘包装器可防止二次 fclose。
        let result = unsafe { libc::fclose(self.0.as_ptr()) };
        std::mem::forget(self);
        if result == 0 {
            Ok(())
        } else {
            Err(crt_error())
        }
    }
}

impl Drop for CrtPipe {
    /// Release an unclosed CRT stream; no parameters or returned value.
    /// 释放尚未关闭的 CRT 流；无参数或返回值。
    fn drop(&mut self) {
        // SAFETY: The wrapper owns one live stream and cannot be dropped twice.
        // 安全性：包装器拥有一个存活流，不可能被释放两次。
        unsafe { libc::fclose(self.0.as_ptr()) };
    }
}

/// Process-tree ownership retained even if pipe conversion or Lua userdata creation fails.
/// 即使管道转换或 Lua userdata 创建失败也会保留的进程树所有权。
struct NativeProcess {
    /// Present until wait/cleanup consumes the complete managed ownership tuple.
    /// 在等待或清理消费完整托管所有权元组之前保持存在。
    ownership: Option<(Child, ManagedChildProcessTree, DetachedChildReaperPermit)>,
}

impl NativeProcess {
    /// Spawn `command` through the shared desktop policy and return a rollback-safe owner or error.
    /// 通过共享桌面策略启动 `command`，返回可安全回滚的所有者或错误。
    fn spawn(command: &mut Command) -> Result<Self, String> {
        Ok(Self {
            ownership: Some(ManagedChildProcessTree::spawn_with_keepalive(
                command,
                "Lua standard process",
                None,
                LuaRuntimeHostMode::Desktop,
            )?),
        })
    }

    /// Wait for the native command contract, then finalize descendants; consumes ownership and returns status/error.
    /// 按原生命令契约等待，再收尾后代进程；消费所有权并返回状态或错误。
    fn wait(mut self) -> Result<ExitStatus, String> {
        // This owner is consumed exactly once by the close/execute operation.
        // 此所有者由关闭或执行操作恰好消费一次。
        let (mut child, tree, permit) = self.ownership.take().expect("native process ownership");
        // Waiting retains the standard synchronous os.execute/pclose behavior.
        // 等待保留标准同步 os.execute/pclose 行为。
        let observed = child.wait();
        // Cleanup runs even when wait fails so no spawned child loses its owner.
        // 即使等待失败也执行清理，确保启动的子进程不丢失所有者。
        let cleanup = finalize_one_shot_process_tree(
            child,
            tree,
            permit,
            observed.as_ref().ok().copied(),
            "Lua standard process",
        );
        match observed {
            Ok(_) => cleanup,
            Err(error) => Err(format!(
                "wait for Lua standard process: {error}; cleanup: {cleanup:?}"
            )),
        }
    }
}

impl Drop for NativeProcess {
    /// Terminate and reap abandoned pipes or partially initialized commands without entering Lua.
    /// 终止并回收被放弃的管道或部分初始化的命令，不进入 Lua。
    fn drop(&mut self) {
        if let Some((child, tree, permit)) = self.ownership.take()
            && let Err(error) =
                finalize_one_shot_process_tree(child, tree, permit, None, "abandoned Lua pipe")
        {
            crate::runtime_logging::warn(error);
        }
    }
}

/// Live pipe resources, ordered so dropping the stream precedes child-process cleanup.
/// 存活管道资源，字段顺序确保先释放流再清理子进程。
struct NativePipeState {
    /// CRT stream preserving streaming, binary/text, numeric-read, and buffering semantics.
    /// 保留流式读写、二进制或文本、数字读取及缓冲语义的 CRT 流。
    stream: CrtPipe,
    /// Managed child and descendant ownership for deterministic close and garbage collection.
    /// 用于确定性关闭及垃圾回收的托管子进程与后代所有权。
    process: NativeProcess,
}

/// Lua file-like userdata used only for desktop native io.popen; ordinary IO stays native.
/// 仅用于桌面原生 io.popen 的 Lua 类文件 userdata；普通 IO 保持原生。
struct NativePipe {
    /// None denotes a closed handle; the mutex serializes the CRT stream across Lua callbacks.
    /// 空值表示已关闭句柄；互斥锁串行化 Lua 回调间的 CRT 流操作。
    state: Mutex<Option<NativePipeState>>,
}

impl Drop for NativePipe {
    /// Reap abandoned children before flushing a discarded writer so a non-reading child cannot block GC.
    /// 刷新被丢弃的写入器前回收被放弃的子进程，避免不读取输入的子进程阻塞垃圾回收。
    /// Consumes the owned state without entering Lua and returns no value.
    /// 不进入 Lua 即消费自有状态，无返回值。
    fn drop(&mut self) {
        if let Some(state) = self.lock().take() {
            drop(state.process);
            drop(state.stream);
        }
    }
}

impl NativePipe {
    /// Lock this handle's state; returns a guard even after poisoning so cleanup ownership is preserved.
    /// 锁定本句柄状态；即使锁中毒也返回守卫，以保留清理所有权。
    fn lock(&self) -> MutexGuard<'_, Option<NativePipeState>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Read each requested Lua format from the live CRT stream, returning values or standard IO errors.
    /// 从存活 CRT 流读取各个 Lua 格式，返回值或标准 IO 错误。
    /// `lua` creates result strings and `formats` contains byte counts or a/l/L/n formats.
    /// `lua` 创建结果字符串，`formats` 包含字节数或 a/l/L/n 格式。
    fn read(&self, lua: &Lua, formats: Variadic<Value>) -> mlua::Result<MultiValue> {
        // The guard keeps the FILE pointer valid throughout all reads.
        // 守卫在全部读取期间保持 FILE 指针有效。
        let guard = self.lock();
        // Closed handles produce Lua errors, matching standard file operations.
        // 已关闭句柄产生 Lua 错误，与标准文件操作一致。
        let state = guard.as_ref().ok_or_else(closed_error)?;
        // SAFETY: Native Lua clears prior read errors before each operation; the guard owns this stream.
        // 安全性：原生 Lua 在每次操作前清除先前读取错误；守卫拥有此流。
        unsafe { clearerr(state.stream.0.as_ptr()) };
        // Default reading is one line without its newline.
        // 默认读取一行并去除换行符。
        let formats = if formats.is_empty() {
            vec![Value::String(lua.create_string("*l")?)]
        } else {
            formats.to_vec()
        };
        // Results stop at the first EOF/numeric-conversion failure.
        // 结果在首次遇到文件结束或数字转换失败时停止。
        let mut results = MultiValue::new();
        for format in formats {
            // Each format is read while the owning mutex remains held.
            // 每个格式都在持有所有者互斥锁期间读取。
            let value = read_format(lua, state.stream.0.as_ptr(), format)?;
            // SAFETY: The stream pointer is protected by the live guard.
            // 安全性：流指针由存活守卫保护。
            if unsafe { libc::ferror(state.stream.0.as_ptr()) } != 0 {
                return io_error_values(lua, crt_error());
            }
            // Nil records EOF or numeric conversion failure for the final format.
            // 空值记录最后一个格式的文件结束或数字转换失败。
            let ended = value.is_nil();
            results.push_back(value);
            if ended {
                break;
            }
        }
        Ok(results)
    }
}

impl UserData for NativePipe {
    /// Register standard file methods; `methods` receives close-gated callbacks and no value is returned.
    /// 注册标准文件方法；`methods` 接收带关闭闸门的回调，无返回值。
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_host_method("read", |lua, pipe, formats: Variadic<Value>| {
            pipe.read(lua, formats)
        });
        methods.add_host_function(
            "write",
            |lua, (handle, values): (AnyUserData, Variadic<Value>)| {
                // Borrowing preserves the original userdata identity returned by successful writes.
                // 借用保留成功写入时返回的原始 userdata 身份。
                let pipe = handle.borrow::<NativePipe>()?;
                // The guard serializes all writes and prevents close during a CRT call.
                // 守卫串行化全部写入，防止 CRT 调用期间关闭。
                let guard = pipe.lock();
                // The exact stream belongs to this userdata, with no candidate object lookup.
                // 精确流属于本 userdata，不进行候选对象查找。
                let state = guard.as_ref().ok_or_else(closed_error)?;
                for value in values {
                    // Lua's coercion accepts only strings and numbers, matching native file:write.
                    // Lua 的转换仅接受字符串和数字，与原生 file:write 一致。
                    let text = lua
                        .coerce_string(value)?
                        .ok_or_else(|| mlua::Error::runtime("string or number expected"))?;
                    // Borrowed bytes remain valid during fwrite, including embedded NUL bytes.
                    // 借用字节在 fwrite 期间保持有效，包括内嵌零字节。
                    let bytes = text.as_bytes();
                    // SAFETY: The guarded FILE and byte slice are valid for this exact length.
                    // 安全性：受保护 FILE 与字节切片在本精确长度范围内有效。
                    if unsafe {
                        libc::fwrite(
                            bytes.as_ptr().cast(),
                            1,
                            bytes.len(),
                            state.stream.0.as_ptr(),
                        )
                    } != bytes.len()
                    {
                        return io_error_values(lua, crt_error());
                    }
                }
                drop(guard);
                drop(pipe);
                handle.into_lua_multi(lua)
            },
        );
        methods.add_host_method("flush", |lua, pipe, ()| {
            // Keep the live FILE locked until its buffered bytes have been flushed.
            // 在缓冲字节刷新完成前保持存活 FILE 的锁。
            let guard = pipe.lock();
            // A closed handle cannot participate in a flush.
            // 已关闭句柄不能参与刷新。
            let state = guard.as_ref().ok_or_else(closed_error)?;
            // SAFETY: The stream is owned and protected for the complete operation.
            // 安全性：流在完整操作期间被拥有和保护。
            if unsafe { libc::fflush(state.stream.0.as_ptr()) } == 0 {
                true.into_lua_multi(lua)
            } else {
                io_error_values(lua, crt_error())
            }
        });
        methods.add_host_method("close", |lua, pipe, ()| {
            // Taking state publishes closure before waiting and makes all aliases observe it.
            // 取出状态在等待前发布关闭，使所有别名都能观察到它。
            let state = pipe.lock().take().ok_or_else(closed_error)?;
            // Preserve a write/close error while still finishing the child process.
            // 保留写入或关闭错误，同时仍然完成子进程收尾。
            let close = state.stream.close();
            // The standard pclose contract waits after closing the corresponding pipe endpoint.
            // 标准 pclose 契约在关闭对应管道端点之后等待。
            let status = state.process.wait().map_err(mlua::Error::runtime)?;
            match close {
                Ok(()) => exit_values(lua, status),
                Err(error) => io_error_values(lua, error),
            }
        });
        methods.add_host_method("seek", |lua, pipe, _: MultiValue| {
            if pipe.lock().is_none() {
                return Err(closed_error());
            }
            io_error_values(lua, ("pipes are not seekable".to_string(), libc::ESPIPE))
        });
        methods.add_host_method(
            "setvbuf",
            |lua, pipe, (mode, size): (String, Option<usize>)| {
                // Only the native buffering modes are accepted; the CRT owns any allocated buffer.
                // 仅接受原生缓冲模式；CRT 拥有任何已分配缓冲区。
                let mode = match mode.as_str() {
                    "full" => libc::_IOFBF,
                    "line" => libc::_IOLBF,
                    "no" => libc::_IONBF,
                    _ => return Err(mlua::Error::runtime("invalid buffering mode")),
                };
                // Guard and state keep the stream alive while its buffering mode changes.
                // 守卫和状态在缓冲模式改变期间保持流存活。
                let guard = pipe.lock();
                // This field is the sole live stream for this handle.
                // 本字段是本句柄唯一存活流。
                let state = guard.as_ref().ok_or_else(closed_error)?;
                // SAFETY: Null requests a CRT-owned buffer and the mode was validated above.
                // 安全性：空指针请求 CRT 自有缓冲区，模式已在上文验证。
                if unsafe {
                    libc::setvbuf(
                        state.stream.0.as_ptr(),
                        std::ptr::null_mut(),
                        mode,
                        size.unwrap_or(8192),
                    )
                } == 0
                {
                    true.into_lua_multi(lua)
                } else {
                    io_error_values(lua, crt_error())
                }
            },
        );
        methods.add_host_function(
            "lines",
            |lua, (handle, formats): (AnyUserData, Variadic<Value>)| {
                if handle.borrow::<NativePipe>()?.lock().is_none() {
                    return Err(closed_error());
                }
                // Retain the userdata until the iterator is collected; EOF does not close file:lines handles.
                // 保留 userdata 直到迭代器被回收；文件结束不会关闭 file:lines 的句柄。
                lua.create_host_function(move |lua, ()| {
                    // Iteration raises IO failures while ordinary read returns nil/message/errno.
                    // 迭代抛出 IO 失败，而普通读取返回空值、消息及 errno。
                    let values = handle.borrow::<NativePipe>()?.read(lua, formats.clone())?;
                    if let (Some(Value::Nil), Some(Value::String(message))) =
                        (values.front(), values.get(1))
                    {
                        return Err(mlua::Error::runtime(message.to_string_lossy()));
                    }
                    Ok(values)
                })
            },
        );
    }
}

/// Read one standard Lua format from `stream`; caller owns its lock; returns one value or format error.
/// 从 `stream` 读取单个标准 Lua 格式；调用方持有其锁；返回单个值或格式错误。
fn read_format(lua: &Lua, stream: *mut libc::FILE, format: Value) -> mlua::Result<Value> {
    // Byte count applies only to numeric formats; textual formats preserve LuaJIT's first-letter dispatch.
    // 字节数仅用于数字格式；文本格式保留 LuaJIT 按首字母分派的行为。
    let (kind, count) = match format {
        Value::Integer(count) if count >= 0 => {
            (b'b', usize::try_from(count).map_err(mlua::Error::external)?)
        }
        Value::Number(count) if count.is_finite() && count >= 0.0 && count < usize::MAX as f64 => {
            (b'b', count as usize)
        }
        Value::String(text) => {
            // An optional leading star is accepted by the configured LuaJIT 5.2 compatibility mode.
            // 当前 LuaJIT 5.2 兼容模式接受可选前导星号。
            let bytes = text.as_bytes();
            // The selected byte is a format tag, never a guessed field or object path.
            // 所选字节是格式标签，绝不是猜测的字段或对象路径。
            let kind = bytes
                .strip_prefix(b"*")
                .unwrap_or(&bytes)
                .first()
                .copied()
                .unwrap_or(0);
            if !matches!(kind, b'a' | b'l' | b'L' | b'n') {
                return Err(mlua::Error::runtime("invalid read format"));
            }
            (kind, 0)
        }
        _ => return Err(mlua::Error::runtime("invalid read format")),
    };
    if kind == b'n' {
        // Match LuaJIT's native fscanf %lf so decimal, hexadecimal, and exponent parsing stay CRT-defined.
        // 对齐 LuaJIT 原生 fscanf %lf，使十进制、十六进制及指数解析继续由 CRT 定义。
        let mut number = 0.0_f64;
        // SAFETY: The caller holds a live FILE; %lf expects exactly the supplied writable f64 pointer.
        // 安全性：调用方持有存活 FILE；%lf 恰好要求所提供的可写 f64 指针。
        return Ok(
            if unsafe { libc::fscanf(stream, c"%lf".as_ptr(), &mut number) } == 1 {
                Value::Number(number)
            } else {
                Value::Nil
            },
        );
    }
    // Chunked reads avoid allocating an untrusted count up front and preserve embedded NUL bytes.
    // 分块读取避免提前按不可信计数分配内存，并保留内嵌零字节。
    let mut bytes = Vec::new();
    if kind == b'b' && count == 0 {
        // SAFETY: fgetc/ungetc inspect EOF without consuming the next byte of the locked stream.
        // 安全性：fgetc/ungetc 检查文件结束，不消费受锁流的下一个字节。
        let byte = unsafe { libc::fgetc(stream) };
        if byte == libc::EOF {
            return Ok(Value::Nil);
        }
        // SAFETY: The byte was just read from this stream and is not EOF.
        // 安全性：该字节刚从本流读取，且不是文件结束标记。
        unsafe { libc::ungetc(byte, stream) };
    } else if matches!(kind, b'l' | b'L') {
        loop {
            // SAFETY: The caller's guard keeps the stream exclusively live during this read.
            // 安全性：调用方守卫在本次读取期间保持流独占存活。
            let byte = unsafe { libc::fgetc(stream) };
            if byte == libc::EOF {
                if bytes.is_empty() {
                    return Ok(Value::Nil);
                }
                break;
            }
            if byte == i32::from(b'\n') {
                if kind == b'L' {
                    bytes.push(b'\n');
                }
                break;
            }
            bytes.push(byte as u8);
        }
    } else {
        // Fixed stack buffer bounds each individual read without limiting the standard *a contract.
        // 固定栈缓冲区限制单次读取大小，不限制标准 *a 契约。
        let mut chunk = [0_u8; 8192];
        loop {
            // The numeric format consumes exactly the remaining count, while *a reads through EOF.
            // 数字格式精确消费剩余计数，而 *a 一直读取到文件结束。
            let wanted = if kind == b'b' {
                (count - bytes.len()).min(chunk.len())
            } else {
                chunk.len()
            };
            if wanted == 0 {
                break;
            }
            // SAFETY: The destination has at least wanted writable bytes and the stream is guarded.
            // 安全性：目标至少具有 wanted 个可写字节，流受守卫保护。
            let read = unsafe { libc::fread(chunk.as_mut_ptr().cast(), 1, wanted, stream) };
            bytes.extend_from_slice(&chunk[..read]);
            if read < wanted {
                break;
            }
        }
        if kind == b'b' && bytes.is_empty() {
            return Ok(Value::Nil);
        }
    }
    Ok(Value::String(lua.create_string(&bytes)?))
}

/// Capture the thread's CRT errno and text before another CRT call; returns owned diagnostics.
/// 在另一项 CRT 调用前捕获线程 CRT errno 及文本；返回自有诊断。
fn crt_error() -> (String, i32) {
    // SAFETY: The CRT returns a valid thread-local errno pointer for the current thread.
    // 安全性：CRT 为当前线程返回有效线程局部 errno 指针。
    let code = unsafe { *_errno() };
    // SAFETY: strerror returns a NUL-terminated CRT message copied before another CRT operation.
    // 安全性：strerror 返回零结尾 CRT 消息，在其他 CRT 操作前复制它。
    let message = unsafe { CStr::from_ptr(libc::strerror(code)) }
        .to_string_lossy()
        .into_owned();
    (message, code)
}

/// Return the standard error for operations on closed file userdata; no parameters are needed.
/// 返回操作已关闭文件 userdata 的标准错误；不需要参数。
fn closed_error() -> mlua::Error {
    mlua::Error::runtime("attempt to use a closed file")
}

/// Encode `error` as Lua's nil/message/errno tuple using `lua`; returns the result values.
/// 使用 `lua` 将 `error` 编码为 Lua 的空值、消息、errno 元组；返回结果值。
fn io_error_values(lua: &Lua, error: (String, i32)) -> mlua::Result<MultiValue> {
    (Option::<bool>::None, error.0, error.1).into_lua_multi(lua)
}

/// Encode the Windows child `status` using LuaJIT 5.2 exit triples; `lua` creates the result values.
/// 使用 LuaJIT 5.2 退出三元组编码 Windows 子进程 `status`；`lua` 创建结果值。
fn exit_values(lua: &Lua, status: ExitStatus) -> mlua::Result<MultiValue> {
    (
        if status.success() { Some(true) } else { None },
        "exit",
        status.code().expect("Windows exit code"),
    )
        .into_lua_multi(lua)
}

/// Build the native Windows command processor invocation for `text`; returns an unspawned command.
/// 为 `text` 构建原生 Windows 命令处理器调用；返回尚未启动的命令。
fn shell_command(text: &str) -> Command {
    // COMSPEC is the documented CRT command-processor source; cmd.exe is its documented missing-value default.
    // COMSPEC 是 CRT 文档规定的命令处理器来源；cmd.exe 是其缺失值默认值。
    let shell = std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into());
    // raw_arg preserves the command expression as CRT system/popen concatenate it after /c.
    // raw_arg 保留命令表达式，与 CRT system/popen 在 /c 后拼接它的行为一致。
    let mut command = Command::new(shell);
    command.arg("/c").raw_arg(text);
    command
}

/// Install native-command replacements in the fresh desktop `lua`; returns unit or an initialization error.
/// 在新的桌面 `lua` 中安装原生命令替代实现；返回空值或初始化错误。
pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    // This is the actual native os table loaded by Lua::unsafe_new before any skill code executes.
    // 这是 Lua::unsafe_new 在任何技能代码执行前加载的实际原生 os 表。
    let os: Table = lua.globals().get("os")?;
    os.set(
        "execute",
        lua.create_host_function(|lua, text: Option<mlua::LuaString>| {
            // LuaJIT's configured 5.2 mode returns true for a missing command without exposing a shell process.
            // 当前 LuaJIT 5.2 模式在命令缺失时返回真，不暴露 shell 进程。
            let Some(text) = text else {
                return true.into_lua_multi(lua);
            };
            // The command string is validated as UTF-8 by the same Rust boundary as managed process APIs.
            // 命令字符串由与托管进程 API 相同的 Rust 边界验证为 UTF-8。
            let mut command = shell_command(&text.to_str()?);
            match NativeProcess::spawn(&mut command) {
                Ok(process) => exit_values(lua, process.wait().map_err(mlua::Error::runtime)?),
                Err(error) => io_error_values(lua, (error, libc::EIO)),
            }
        })?,
    )?;
    // Native io remains authoritative for filesystem handles and standard streams.
    // 原生 io 继续作为文件系统句柄与标准流的权威实现。
    let io: Table = lua.globals().get("io")?;
    // The opener changes only process creation and preserves streaming rather than buffering until child exit.
    // 打开器仅改变进程创建，保留流式行为，不缓冲到子进程退出。
    let opener =
        lua.create_host_function(|lua, (text, mode): (mlua::LuaString, Option<String>)| {
            // Native Windows popen supports r/w with optional text or binary mode.
            // 原生 Windows popen 支持 r/w 以及可选文本或二进制模式。
            let (write, binary) = match mode.as_deref().unwrap_or("r") {
                "r" | "rt" => (false, false),
                "rb" => (false, true),
                "w" | "wt" => (true, false),
                "wb" => (true, true),
                _ => return Err(mlua::Error::runtime("invalid popen mode")),
            };
            // Only the selected endpoint is piped; the other standard handles retain native inheritance.
            // 仅所选端点使用管道，其他标准句柄保留原生继承。
            let mut command = shell_command(&text.to_str()?);
            if write {
                command.stdin(Stdio::piped());
            } else {
                command.stdout(Stdio::piped());
            }
            // The rollback owner exists before transferring the child pipe into the CRT.
            // 将子进程管道转移到 CRT 之前，回滚所有者已经存在。
            let mut process = match NativeProcess::spawn(&mut command) {
                Ok(process) => process,
                Err(error) => return io_error_values(lua, (error, libc::EIO)),
            };
            // This child is the unique tuple member returned by the shared spawn operation.
            // 此子进程是共享启动操作返回的唯一元组成员。
            let child = &mut process.ownership.as_mut().expect("new native process").0;
            // SAFETY: into_raw_handle transfers sole ownership of the selected pipe to OwnedHandle.
            // 安全性：into_raw_handle 将所选管道的唯一所有权转移到 OwnedHandle。
            let handle = unsafe {
                OwnedHandle::from_raw_handle(if write {
                    child.stdin.take().expect("piped stdin").into_raw_handle()
                } else {
                    child.stdout.take().expect("piped stdout").into_raw_handle()
                })
            };
            // Failed CRT conversion drops both the handle and managed process owner.
            // CRT 转换失败时会释放句柄及托管进程所有者。
            let stream = match CrtPipe::from_handle(handle, write, binary) {
                Ok(stream) => stream,
                Err(error) => return io_error_values(lua, error),
            };
            lua.create_userdata(NativePipe {
                state: Mutex::new(Some(NativePipeState { stream, process })),
            })?
            .into_lua_multi(lua)
        })?;
    // A classifier lets the Lua facade dispatch only our userdata and preserve native IO for every other value.
    // 分类器让 Lua 外观仅分派我们的 userdata，并对所有其他值保留原生 IO。
    let classifier =
        lua.create_host_function(|_, value: Value| -> mlua::Result<Option<&'static str>> {
            match value {
                Value::UserData(handle) if handle.is::<NativePipe>() => {
                    Ok(Some(if handle.borrow::<NativePipe>()?.lock().is_some() {
                        "file"
                    } else {
                        "closed file"
                    }))
                }
                _ => Ok(None),
            }
        })?;
    // The facade is bundled source, not skill-provided code, and captures native functions before replacement.
    // 外观为内置源码而非技能提供的代码，在替换前捕获原生函数。
    let installer: Function = lua.load(include_str!("native_process_io.lua")).eval()?;
    installer.call::<()>((io, opener, classifier))
}
