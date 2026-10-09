{ self }:
{ config, lib, pkgs, ... }:
let
  cfg = config.programs.appwarm;
  applications = lib.mapAttrsToList (name: app: { inherit name app; }) cfg.applications;
  stagedApplications = builtins.filter ({ app, ... }: app.stage.enable) applications;
  stageEntry = { name, app }:
    lib.concatStringsSep "|" ([
      name
      app.stage.appId
      app.stage.desktopId
      (if app.stage.command == null then lib.getExe app.package else app.stage.command)
    ] ++ app.stage.arguments);
  stageEntries = cfg.stages ++ map stageEntry stagedApplications;
  hasStages = stageEntries != [ ];
  warmApps = lib.unique (cfg.apps ++ map ({ name, ... }: name)
    (builtins.filter ({ app, ... }: app.warm) applications));
  stageEnv = "APPWARM_DEFAULT_STAGES=${lib.concatStringsSep ";" stageEntries}";
  appEnv = "APPWARM_DEFAULT_APPS=${lib.concatStringsSep "," warmApps}";
  settingsEnv = lib.mapAttrsToList (name: value:
    "APPWARM_DEFAULT_${lib.toUpper name}=${toString value}") cfg.settings;
  validName = name: builtins.match "[A-Za-z0-9_-][A-Za-z0-9_.-]*" name != null
    && builtins.stringLength name <= 64 && !lib.hasInfix ".." name;
  validAppId = id: builtins.match "[A-Za-z0-9_.-]+" id != null
    && builtins.stringLength id <= 128;
  validField = value: value != "" && builtins.match "[^|;\n\r]*" value != null;
  applicationType = lib.types.coercedTo lib.types.package
    (package: { inherit package; })
    (lib.types.submodule ({ name, ... }: {
      options = {
        package = lib.mkOption {
          type = lib.types.package;
          description = "Package to install and use for this application.";
        };
        warm = lib.mkOption {
          type = lib.types.bool;
          default = true;
          description = "Include this application's learned profile in page-cache warming.";
        };
        stage = {
          enable = lib.mkEnableOption "hidden execution staging for this application";
          appId = lib.mkOption {
            type = lib.types.str;
            default = name;
            description = "Wayland app ID expected from the staged window.";
          };
          desktopId = lib.mkOption {
            type = lib.types.str;
            default = "${name}.desktop";
            description = "Desktop entry to wrap for staged launches.";
          };
          command = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            description = "Executable to stage. Defaults to the package's main executable.";
          };
          arguments = lib.mkOption {
            type = lib.types.listOf lib.types.str;
            default = [ ];
            description = "Arguments passed to the staged executable.";
          };
        };
      };
    }));
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
      description = "Legacy application profile names to warm through the page cache. Prefer applications.";
    };
    stages = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "firefox|firefox|firefox.desktop|firefox" ];
      description = "Legacy execution stage entries: profile|app ID|desktop file|command|arguments. Prefer applications.<name>.stage.";
    };
    applications = lib.mkOption {
      type = lib.types.attrsOf applicationType;
      default = { };
      example = lib.literalExpression ''
        {
          firefox = pkgs.firefox;
          code = {
            package = pkgs.vscode;
            stage.enable = true;
            stage.appId = "code";
            stage.desktopId = "code.desktop";
            stage.arguments = [ "--new-window" ];
          };
        }
      '';
      description = "Applications to install and warm. A package value uses the application name for its profile; a detailed value can enable hidden staging.";
    };
    settings = {
      window_sec = lib.mkOption {
        type = lib.types.ints.between 1 3600;
        default = 10;
        description = "Learning window in seconds.";
      };
      budget_mib = lib.mkOption {
        type = lib.types.ints.between 1 4096;
        default = 256;
        description = "Maximum page-cache requests per warm run, in MiB.";
      };
      min_available_mib = lib.mkOption {
        type = lib.types.ints.between 1 1048576;
        default = 1024;
        description = "Minimum available RAM for warming or staging, in MiB.";
      };
      max_file_mib = lib.mkOption {
        type = lib.types.ints.between 1 4096;
        default = 16;
        description = "Maximum learned range per file, in MiB.";
      };
      stage_budget_mib = lib.mkOption {
        type = lib.types.ints.between 1 16384;
        default = 2048;
        description = "Maximum RAM for all frozen staged applications, in MiB.";
      };
      stage_settle_ms = lib.mkOption {
        type = lib.types.ints.between 1 30000;
        default = 2000;
        description = "Delay after the first staged window appears, in milliseconds.";
      };
      restage_delay_sec = lib.mkOption {
        type = lib.types.ints.between 1 300;
        default = 5;
        description = "Seconds to wait after all application windows close before staging it again.";
      };
    };
    niri = {
      enable = lib.mkEnableOption "the patched Niri compositor needed for hidden execution staging";
      prebuilt = lib.mkOption {
        type = lib.types.bool;
        default = pkgs.stdenv.hostPlatform.system == "x86_64-linux";
        description = "Use the compact prebuilt patched Niri package on x86_64 Linux. Disable to compile it locally.";
      };
      basePackage = lib.mkOption {
        type = lib.types.nullOr lib.types.package;
        default = null;
        description = "Custom Niri base package to patch; takes precedence over the prebuilt package.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = lib.concatMap ({ name, app }:
      [{
        assertion = validName name;
        message = "programs.appwarm.applications.${name}: profile name must contain only letters, digits, _, -, or . and be at most 64 characters.";
      }] ++ lib.optionals app.stage.enable [{
        assertion = validAppId app.stage.appId
          && validName app.stage.desktopId
          && lib.hasSuffix ".desktop" app.stage.desktopId
          && validField (if app.stage.command == null then lib.getExe app.package else app.stage.command)
          && lib.all validField app.stage.arguments;
        message = "programs.appwarm.applications.${name}.stage has an invalid appId, desktopId, command, or argument. Use a valid .desktop filename and nonempty command fields without |, ;, or newlines.";
      }]) applications;

    environment.systemPackages = [ cfg.package pkgs.strace ]
      ++ map ({ app, ... }: app.package) applications;

    programs.niri.package = lib.mkIf cfg.niri.enable
      (if cfg.niri.basePackage != null then
        pkgs.callPackage ./niri-appwarm.nix { niri = cfg.niri.basePackage; }
      else if cfg.niri.prebuilt && pkgs.stdenv.hostPlatform.system == "x86_64-linux" then
        self.packages.${pkgs.stdenv.hostPlatform.system}.niri-appwarm-prebuilt
      else
        self.packages.${pkgs.stdenv.hostPlatform.system}.niri-appwarm);

    systemd.user.services.appwarm = {
      description = "Warm selected application startup files";
      after = [ "graphical-session.target" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${cfg.package}/bin/appwarm warm-all";
        Environment = [ appEnv ] ++ settingsEnv;
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

    systemd.user.services.appwarm-desktop = lib.mkIf hasStages {
      description = "Integrate staged apps with desktop launchers";
      wantedBy = [ "default.target" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${cfg.package}/bin/appwarm desktop-sync";
        Environment = [ stageEnv ] ++ settingsEnv;
      };
    };

    systemd.user.services.appwarm-stage = lib.mkIf hasStages {
      description = "Stage selected apps after login";
      after = [ "graphical-session.target" ];
      path = [ config.programs.niri.package ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${cfg.package}/bin/appwarm stage-all";
        Environment = [ stageEnv ] ++ settingsEnv;
        Nice = 19;
        IOSchedulingClass = "idle";
        CPUWeight = 1;
        IOWeight = 1;
      };
    };

    systemd.user.timers.appwarm-stage = lib.mkIf hasStages {
      description = "Delay execution staging until after login";
      wantedBy = [ "graphical-session.target" ];
      timerConfig = {
        OnActiveSec = "2min";
        AccuracySec = "15s";
        Unit = "appwarm-stage.service";
      };
    };

    systemd.user.services.appwarm-monitor = lib.mkIf hasStages {
      description = "Monitor staged apps, memory pressure, and app closure";
      wantedBy = [ "graphical-session.target" ];
      after = [ "graphical-session.target" ];
      partOf = [ "graphical-session.target" ];
      path = [ config.programs.niri.package ];
      serviceConfig = {
        Type = "simple";
        ExecStart = "${cfg.package}/bin/appwarm monitor";
        Environment = [ stageEnv ] ++ settingsEnv;
        Restart = "on-failure";
        RestartSec = "5s";
        Nice = 19;
        CPUWeight = 1;
        MemoryMax = "64M";
      };
    };
  };
}
