program Main;
uses Greeter;
var
  G: TGreeter;
begin
  G := DefaultGreeter;
  WriteLn(G.Greet);
  G.Free;
end.
