/// Adds two numbers together
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

/// Subtracts b from a
pub fn subtract(a: i32, b: i32) -> i32 {
    a - b
}

/// A simple calculator
pub struct Calculator {
    value: i32,
}

impl Calculator {
    /// Creates a new calculator
    pub fn new() -> Self {
        Self { value: 0 }
    }

    /// Adds to the current value
    pub fn add(&mut self, x: i32) {
        self.value += x;
    }

    /// Gets the current value
    pub fn get(&self) -> i32 {
        self.value
    }
}
