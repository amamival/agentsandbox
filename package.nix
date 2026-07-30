{ lib, rustPlatform, makeWrapper, openssh, util-linux, libvirt, virtiofsd, vulnix }:
let
  cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
in
rustPlatform.buildRustPackage rec {
  pname = cargoToml.package.name;
  version = cargoToml.package.version;
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./src ./template ./man ];
  };
  cargoLock.lockFile = ./Cargo.lock;
  nativeBuildInputs = [ makeWrapper ];
  postInstall = "install -D man/devvm.1 $out/share/man/man1/devvm.1";
  # We expect that libvirtd and qemu are configured on host. See template/configuration.nix
  postFixup = ''
    wrapProgram "$out/bin/${pname}" \
      --prefix PATH : ${lib.makeBinPath [ openssh util-linux libvirt virtiofsd vulnix ]}
  '';
  meta = {
    description = cargoToml.package.description;
    license = lib.licenses.mit;
    mainProgram = pname;
    platforms = [ "x86_64-linux" ];
  };
}
