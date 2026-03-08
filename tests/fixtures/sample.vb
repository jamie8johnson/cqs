Imports System

Namespace SampleApp
    Public Class Calculator
        Public Function Add(a As Integer, b As Integer) As Integer
            Return a + b
        End Function

        Public Function Multiply(a As Integer, b As Integer) As Integer
            Return a * b
        End Function
    End Class

    Public Enum Operation
        Add
        Subtract
    End Enum
End Namespace
