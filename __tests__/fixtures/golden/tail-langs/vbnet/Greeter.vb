Imports System
Imports System.Collections.Generic
Namespace Demo
    Public Interface IGreeter
        Function Greet(name As String) As String
    End Interface
    Public Class Greeter
        Implements IGreeter
        Private ReadOnly _prefix As String
        Public Event Greeted(message As String)
        Public Sub New(prefix As String)
            _prefix = prefix
        End Sub
        Public Function Greet(name As String) As String Implements IGreeter.Greet
            Dim message = $"{_prefix}, {Capitalize(name)}"
            RaiseEvent Greeted(message)
            Return message
        End Function
        Private Function Capitalize(s As String) As String
            Return s.Substring(0, 1).ToUpper() & s.Substring(1)
        End Function
        Public Shared Function Default() As Greeter
            Return New Greeter("Hello")
        End Function
    End Class
End Namespace
