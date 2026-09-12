{ pkgs ? import <nixpkgs> {} }:
let
  lib = pkgs.lib;
  mkGreeting = name: "Hello, ${name}";
  names = [ "alice" "bob" ];
in rec {
  greetings = map mkGreeting names;
  shout = s: lib.toUpper s;
  loud = map shout greetings;
  package = pkgs.stdenv.mkDerivation {
    pname = "greeter";
    version = "0.1";
    src = ./.;
    buildPhase = "echo ${builtins.concatStringsSep \", \" loud} > out.txt";
  };
}
