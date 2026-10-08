{ self }:
{ config, lib, pkgs, ... }:
let
  cfg = config.programs.appwarm;
  stageEnv = "APPWARM_DEFAULT_STAGES=${lib.concatStringsSep ";" cfg.stages}";
  appEnv = "APPWARM_DEFAULT_APPS=${lib.concatStringsSep "," cfg.apps}";
in
{
  options.programs.appwarm = {
    enable = lib.mkEnableOption "Appwarm startup optimization";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.appwarm;
      defaultText = "appwarm.packages.<system>.appwarm";
      description = "Appwarm executable. Set this to a prebuilt release package to avoid compiling Rust locally.";
    };
    apps = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "Application profile names to warm through the page cache.";
    };
    stages = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "firefox|firefox|firefox.desktop|firefox" ];
      description = "Execution stage entries: profile|app ID|desktop file|command|arguments, separated by vertical bars.";
    };
    niri = {
      enable = lib.mkEnableOption "the patched Niri compositor needed for hidden execution staging";
      basePackage = lib.mkOption {
        type = lib.types.package;
        default = pkgs.niri;
        defaultText = "pkgs.niri";
        description = "Niri package to override with the pinned hidden-workspace source and cgroup patch.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    programs.niri.package = lib.mkIf cfg.niri.enable
      (pkgs.callPackage ./niri-appwarm.nix { niri = cfg.niri.basePackage; });

    systemd.user.services.appwarm = {
      description = "Warm selected application startup files";
      after = [ "graphical-session.target" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${cfg.package}/bin/appwarm warm-all";
        Environment = appEnv;
        Nice = 19;
        IOSchedulingClass = "idle";
        CPUWeight = 1;
        IOWeight = 1;
        MemoryMax = "512M";
      };
    };

    systemd.user.timers.appwarm = {
      description = "Delay page-cache warming until after login";
      wantedBy = [ "default.target" ];
      timerConfig = {
        OnActiveSec = "2min";
        OnUnitActiveSec = "2h";
        AccuracySec = "1min";
        Unit = "appwarm.service";
      };
    };

    systemd.user.services.appwarm-desktop = {
      description = "Integrate staged apps with desktop launchers";
      wantedBy = [ "default.target" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${cfg.package}/bin/appwarm desktop-sync";
        Environment = stageEnv;
      };
    };

    systemd.user.services.appwarm-stage = {
      description = "Stage selected apps after login";
      after = [ "graphical-session.target" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${cfg.package}/bin/appwarm stage-all";
        Environment = stageEnv;
        Nice = 19;
        IOSchedulingClass = "idle";
        CPUWeight = 1;
        IOWeight = 1;
      };
    };

    systemd.user.timers.appwarm-stage = {
      description = "Delay execution staging until after login";
      wantedBy = [ "default.target" ];
      timerConfig = {
        OnActiveSec = "2min";
        AccuracySec = "15s";
        Unit = "appwarm-stage.service";
      };
    };

    systemd.user.services.appwarm-monitor = {
      description = "Evict frozen Appwarm apps under memory pressure";
      wantedBy = [ "default.target" ];
      serviceConfig = {
        Type = "simple";
        ExecStart = "${cfg.package}/bin/appwarm monitor";
        Restart = "on-failure";
        RestartSec = "5s";
        Nice = 19;
        CPUWeight = 1;
        MemoryMax = "64M";
      };
    };
  };
}
