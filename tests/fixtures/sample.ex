defmodule Calculator do
  @moduledoc "A simple calculator module"

  @doc "Add two numbers"
  def add(a, b) do
    a + b
  end

  @doc "Subtract two numbers"
  def subtract(a, b) do
    a - b
  end

  defp validate(n) when is_number(n), do: :ok
  defp validate(_), do: {:error, :not_a_number}
end

defprotocol Printable do
  @doc "Convert to printable string"
  def to_string(data)
end

defimpl Printable, for: Integer do
  def to_string(n), do: Integer.to_string(n)
end

defmodule Greeter do
  defmacro say_hello(name) do
    quote do
      IO.puts("Hello, #{unquote(name)}")
    end
  end

  def greet(name) do
    name
    |> String.trim()
    |> format_greeting()
  end

  defp format_greeting(name) do
    "Hello, #{name}!"
  end
end
