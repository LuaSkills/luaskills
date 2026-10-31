-- Install desktop process pipes into the native IO table; parameters are the IO table, opener, and pipe classifier.
-- 将桌面进程管道安装到原生 IO 表；参数为 IO 表、打开器及管道分类器。
-- Returns no values; native filesystem files and standard streams retain their original implementations.
-- 不返回值；原生文件系统文件及标准流保留原有实现。
return function(io, open_pipe, pipe_type)
    -- Original functions are captured before replacing dispatch entry points.
    -- 在替换分派入口之前捕获原始函数。
    local native_type, native_close = io.type, io.close
    -- Original default-stream selectors remain authoritative for native file handles.
    -- 原始默认流选择器继续作为原生文件句柄的权威实现。
    local native_input, native_output = io.input, io.output
    -- Original IO operations handle every stream not created by the desktop pipe opener.
    -- 原始 IO 操作处理非桌面管道打开器创建的所有流。
    local native_read, native_write, native_flush, native_lines = io.read, io.write, io.flush, io.lines
    -- Optional default pipe handles exist only after explicit io.input/io.output selection.
    -- 可选默认管道句柄仅在显式 io.input/io.output 选择后存在。
    local pipe_input, pipe_output

    io.popen = open_pipe

    -- Classify value using its exact userdata type; returns a file label or the native classifier result.
    -- 使用值的精确 userdata 类型分类；返回文件标签或原生分类器结果。
    io.type = function(value)
        -- The classifier returns nil only for values outside the explicitly supported custom pipe type.
        -- 分类器仅对显式支持的自定义管道类型之外的值返回空值。
        local kind = pipe_type(value)
        if kind ~= nil then return kind end
        return native_type(value)
    end

    -- Close an explicit handle or selected default output; returns the underlying close results.
    -- 关闭显式句柄或选定默认输出；返回底层关闭结果。
    io.close = function(value)
        if value == nil and pipe_output ~= nil then return pipe_output:close() end
        if pipe_type(value) ~= nil then return value:close() end
        return native_close(value)
    end

    -- Select or query the default input; value is a filename or file, and the selected file is returned.
    -- 选择或查询默认输入；value 为文件名或文件，返回所选文件。
    io.input = function(value)
        if value == nil then
            if pipe_input ~= nil then return pipe_input end
            return native_input()
        end
        -- Only custom pipe userdata is handled here; closed files fail before changing the default.
        -- 此处仅处理自定义管道 userdata；已关闭文件在改变默认值前失败。
        local kind = pipe_type(value)
        if kind ~= nil then
            if kind == "closed file" then error("attempt to use a closed file", 2) end
            pipe_input = value
            return value
        end
        -- Keep the previous pipe selection when the native selector rejects its argument.
        -- 原生选择器拒绝参数时保留先前管道选择。
        local selected = native_input(value)
        pipe_input = nil
        return selected
    end

    -- Select or query the default output; value is a filename or file, and the selected file is returned.
    -- 选择或查询默认输出；value 为文件名或文件，返回所选文件。
    io.output = function(value)
        if value == nil then
            if pipe_output ~= nil then return pipe_output end
            return native_output()
        end
        -- Custom output pipes share the exact same closed-state contract as custom input pipes.
        -- 自定义输出管道与自定义输入管道共享精确相同的关闭状态契约。
        local kind = pipe_type(value)
        if kind ~= nil then
            if kind == "closed file" then error("attempt to use a closed file", 2) end
            pipe_output = value
            return value
        end
        -- Native selection is committed before releasing the previous custom default.
        -- 在释放先前自定义默认值前，先提交原生选择。
        local selected = native_output(value)
        pipe_output = nil
        return selected
    end

    -- Read formats from the selected default input; returns all standard read values.
    -- 从所选默认输入读取各格式；返回全部标准读取值。
    io.read = function(...)
        if pipe_input ~= nil then return pipe_input:read(...) end
        return native_read(...)
    end

    -- Write values to the selected default output; returns that file or IO error values.
    -- 将各值写入所选默认输出；返回该文件或 IO 错误值。
    io.write = function(...)
        if pipe_output ~= nil then return pipe_output:write(...) end
        return native_write(...)
    end

    -- Flush the selected default output without arguments; returns success or IO error values.
    -- 无参数刷新所选默认输出；返回成功或 IO 错误值。
    io.flush = function()
        if pipe_output ~= nil then return pipe_output:flush() end
        return native_flush()
    end

    -- Iterate a filename or selected default input using the supplied formats; returns an iterator.
    -- 使用所提供格式迭代文件名或选定默认输入；返回迭代器。
    io.lines = function(filename, ...)
        if filename == nil and pipe_input ~= nil then return pipe_input:lines(...) end
        return native_lines(filename, ...)
    end
end
