# Calculator module
module Calculator

export add, multiply, factorial

function add(x::Int, y::Int)::Int
    return x + y
end

function multiply(x, y)
    return x * y
end

function factorial(n::Int)
    if n <= 1
        return 1
    else
        return n * factorial(n - 1)
    end
end

end # module

# Point struct
struct Point
    x::Float64
    y::Float64
end

# Abstract type
abstract type Shape end

function greet(name::String)
    println("Hello, $name!")
end

function distance(p1::Point, p2::Point)
    dx = p1.x - p2.x
    dy = p1.y - p2.y
    return sqrt(dx^2 + dy^2)
end
