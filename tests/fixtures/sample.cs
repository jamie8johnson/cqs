using System;

namespace SampleApp
{
    public class Calculator
    {
        public int Add(int a, int b)
        {
            return a + b;
        }

        public int Multiply(int a, int b)
        {
            return a * b;
        }
    }

    public enum Operation
    {
        Add,
        Subtract
    }
}
