using Xunit;

namespace HelloTests;

public static class Calculator
{
    public static int Add(int a, int b)
    {
        var sum = a + b;
        return sum;
    }
}

public class CalculatorTests
{
    [Fact]
    public void Adds()
    {
        Assert.Equal(5, Calculator.Add(2, 3));
    }

    [Fact]
    public void Fails()
    {
        Assert.Equal(5, Calculator.Add(2, 2));
    }

    [Theory]
    [InlineData(1, 1, 2)]
    [InlineData(2, 2, 4)]
    public void AddsMany(int a, int b, int expected)
    {
        Assert.Equal(expected, Calculator.Add(a, b));
    }

    [Theory]
    [InlineData("2023-09-25T14:30:00+02:00")]
    [InlineData("a string with spaces")]
    [InlineData("a \"quoted\" string")]
    [InlineData(@"C:\some folder\file.txt")]
    [InlineData("")]
    public void StringRow(string input)
    {
        Assert.NotNull(input);
    }
}
