"""Sample Python module for testing."""

def greet(name: str) -> str:
    """Return a greeting for the given name."""
    return f"Hello, {name}!"

def calculate_sum(numbers: list[int]) -> int:
    """Calculate the sum of a list of numbers."""
    return sum(numbers)

class Counter:
    """A simple counter class."""

    def __init__(self, start: int = 0):
        """Initialize counter with a starting value."""
        self.value = start

    def increment(self):
        """Increment the counter by 1."""
        self.value += 1

    def decrement(self):
        """Decrement the counter by 1."""
        self.value -= 1

    def get(self) -> int:
        """Get the current counter value."""
        return self.value
