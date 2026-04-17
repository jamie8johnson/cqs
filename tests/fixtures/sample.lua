-- Sample Lua file for parser tests

-- Greeting function
function greet(name)
    print("Hello, " .. name .. "!")
end

-- Mathematical utility
local function fibonacci(n)
    if n <= 1 then return n end
    return fibonacci(n - 1) + fibonacci(n - 2)
end

-- String processing
function format_name(first, last)
    return string.upper(first) .. " " .. last
end
