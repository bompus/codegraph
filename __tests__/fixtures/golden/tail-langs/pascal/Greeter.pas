unit Greeter;
interface
type
  TGreeter = class
  private
    FName: string;
    function Capitalize(const S: string): string;
  public
    constructor Create(const AName: string);
    function Greet: string;
    property Name: string read FName write FName;
  end;
function DefaultGreeter: TGreeter;
implementation
uses SysUtils;
constructor TGreeter.Create(const AName: string);
begin
  inherited Create;
  FName := AName;
end;
function TGreeter.Capitalize(const S: string): string;
begin
  Result := UpperCase(Copy(S, 1, 1)) + Copy(S, 2, Length(S));
end;
function TGreeter.Greet: string;
begin
  Result := 'Hello, ' + Capitalize(FName);
end;
function DefaultGreeter: TGreeter;
begin
  Result := TGreeter.Create('world');
end;
end.
