{ fetchFromGitHub, niri }:

# The pinned hidden-workspaces branch is based on niri 26.04 and adds a
# workspace that is absent from the overview, workspace IPC, and navigation.
# Keep this separate from nixpkgs' niri so the patch can be removed cleanly.
niri.overrideAttrs (old: {
  src = fetchFromGitHub {
    owner = "argosnothing";
    repo = "niri";
    rev = "7969cb499f0b813e20cc247dc1a1598b0f3bcb6f";
    hash = "sha256-TIm5+EHkCxwQipcoTkeDWhNOXdcgHvNe1L+/hACgbFM=";
  };

  # Route clients by the transient systemd unit that launched them. This is
  # available before an application sets its xdg_toplevel app ID.
  patches = (old.patches or [ ]) ++ [ ../patches/niri-appwarm-cgroup.patch ];

  env = old.env // {
    NIRI_BUILD_COMMIT = "appwarm-hidden-7969cb4";
  };
})
