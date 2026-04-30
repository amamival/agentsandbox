{ lib, rustPlatform, makeWrapper, libvirt, openssh, util-linux, virtiofsd, vulnix }:
let
  cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
  vulnix_ = vulnix.overrideAttrs (_: { patches = [ ./vulnix-1.12.1-storedir.patch ]; });
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
  postInstall = "install -D man/agentsandbox.1 $out/share/man/man1/agentsandbox.1";
  postFixup = ''
    wrapProgram "$out/bin/${pname}" \
      --prefix PATH : ${lib.makeBinPath [ libvirt openssh util-linux virtiofsd vulnix_ ]}
  '';
  meta = {
    description = cargoToml.package.description;
    license = lib.licenses.mit;
    mainProgram = pname;
    platforms = [ "x86_64-linux" ];
  };
}
