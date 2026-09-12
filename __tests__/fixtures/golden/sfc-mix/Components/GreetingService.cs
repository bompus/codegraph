public record GreetRequest(string Name);
public class GreetingService
{
    public string Greet(GreetRequest req) => $"Hello, {req.Name}!";
}
