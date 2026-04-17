import Foundation

/// A point in 2D space.
struct Point {
    var x: Double
    var y: Double

    func distanceTo(_ other: Point) -> Double {
        let dx = x - other.x
        let dy = y - other.y
        return (dx * dx + dy * dy).squareRoot()
    }
}

/// Protocol for shapes that can compute area.
protocol Shape {
    func area() -> Double
    func perimeter() -> Double
}

/// A circle shape.
class Circle: Shape {
    let center: Point
    let radius: Double

    init(center: Point, radius: Double) {
        self.center = center
        self.radius = radius
    }

    func area() -> Double {
        return Double.pi * radius * radius
    }

    func perimeter() -> Double {
        return 2 * Double.pi * radius
    }
}

enum Direction {
    case north, south, east, west

    func opposite() -> Direction {
        switch self {
        case .north: return .south
        case .south: return .north
        case .east: return .west
        case .west: return .east
        }
    }
}

func greet(name: String) -> String {
    return "Hello, \(name)!"
}
