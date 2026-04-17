class Calculator
  def add(a, b)
    a + b
  end

  def multiply(a, b)
    a * b
  end
end

module MathHelpers
  def self.factorial(n)
    return 1 if n <= 1
    n * factorial(n - 1)
  end
end
