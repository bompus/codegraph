<cfset greeter = new Greeter("Hello")>
<cfset message = greeter.greet("world")>
<cfquery name="users" datasource="app">
    SELECT id, name FROM users WHERE name = <cfqueryparam value="#message#" cfsqltype="cf_sql_varchar">
</cfquery>
<cfscript>
    function shout(required string s) { return uCase(arguments.s); }
    loud = shout(message);
</cfscript>
<cfoutput>#loud#</cfoutput>
