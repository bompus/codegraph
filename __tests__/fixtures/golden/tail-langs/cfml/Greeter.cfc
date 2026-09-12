component extends="BaseGreeter" implements="IGreeter" {
    property name="prefix" type="string";
    public function init(required string prefix) {
        variables.prefix = arguments.prefix;
        return this;
    }
    public string function greet(required string name) {
        var message = variables.prefix & ", " & capitalize(arguments.name);
        logIt(message);
        return message;
    }
    private string function capitalize(required string s) {
        return uCase(left(arguments.s, 1)) & mid(arguments.s, 2, len(arguments.s));
    }
}
