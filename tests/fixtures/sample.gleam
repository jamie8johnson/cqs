import gleam/io
import gleam/int

/// Add two integers
pub fn add(x: Int, y: Int) -> Int {
  x + y
}

/// Greet a person by name
pub fn greet(name: String) -> String {
  "Hello, " <> name <> "!"
}

/// Represents a color
pub type Color {
  Red
  Green
  Blue
}

/// A 2D point
pub type Point {
  Point(x: Float, y: Float)
}

/// Type alias for user IDs
pub type UserId = Int

/// Maximum number of retries
pub const max_retries: Int = 3

/// Recursive factorial
fn factorial(n: Int) -> Int {
  case n {
    0 -> 1
    _ -> n * factorial(n - 1)
  }
}

/// Entry point
pub fn main() {
  let result = add(1, 2)
  io.println(int.to_string(result))
}
