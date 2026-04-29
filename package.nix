{lib, rustPlatform, makeWrapper, libvirt, openssh, util-linux, virtiofsd}:
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
  doCheck = false; # Cannot have a nested container.
  postInstall = "install -D ${./man/agentsandbox.1} $out/share/man/man1/agentsandbox.1";
  postFixup = ''
    wrapProgram "$out/bin/${pname}" \
      --prefix PATH : ${lib.makeBinPath [ libvirt openssh util-linux virtiofsd ]}
  '';
  meta = {
    description = cargoToml.package.description;
    license = lib.licenses.mit;
    mainProgram = pname;
    platforms = [ "x86_64-linux" ];
  };
}
