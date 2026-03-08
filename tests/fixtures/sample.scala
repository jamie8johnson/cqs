package sample

class Calculator {
  def add(a: Int, b: Int): Int = a + b
  def multiply(a: Int, b: Int): Int = a * b
}

trait Printable {
  def prettyPrint(): String
}

object MathUtils {
  def factorial(n: Int): Int =
    if (n <= 1) 1 else n * factorial(n - 1)
}
