{ config, lib, ... }:
with lib;
{
  options.services.greeter = {
    enable = mkEnableOption "greeter";
    name = mkOption { type = types.str; default = "world"; };
  };
  config = mkIf config.services.greeter.enable {
    environment.etc."greeting".text = "Hello, ${config.services.greeter.name}";
  };
}
