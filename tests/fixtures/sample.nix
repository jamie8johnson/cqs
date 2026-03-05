# A sample Nix derivation for testing
{ pkgs ? import <nixpkgs> {} }:

let
  # Configuration
  version = "1.0.0";
  name = "hello-world";
in
{
  # Build the hello-world package
  mkHello = { stdenv, fetchurl }:
    stdenv.mkDerivation {
      pname = name;
      inherit version;
      src = fetchurl {
        url = "https://example.com/hello-${version}.tar.gz";
        sha256 = "0000000000000000000000000000000000000000000000000000";
      };
      buildPhase = ''
        make
      '';
      installPhase = ''
        mkdir -p $out/bin
        cp hello $out/bin/
      '';
    };

  # Utility to greet a user
  greet = user:
    "Hello, ${user}! Welcome to Nix.";

  # Configuration attribute set
  config = {
    enableTests = true;
    features = [ "logging" "metrics" ];
    port = 8080;
  };

  # Recursive helper functions
  helpers = rec {
    double = x: x * 2;
    quadruple = x: double (double x);
  };

  # Shell environment for development
  devShell = pkgs.mkShell {
    buildInputs = with pkgs; [
      rustc
      cargo
      pkg-config
    ];
    shellHook = ''
      echo "Development environment loaded"
    '';
  };

  # NixOS module
  nixosModule = { config, lib, ... }: {
    options.services.hello = {
      enable = lib.mkEnableOption "hello service";
      port = lib.mkOption {
        type = lib.types.port;
        default = 8080;
        description = "Port to listen on";
      };
    };
  };
}
