{ lib
, rustPlatform
, fetchFromGitHub
, makeWrapper
, libvirt
, openssh
, util-linux
, virtiofsd
,
}:
rustPlatform.buildRustPackage rec {
  pname = "agentsandbox";
  version = "0.1.0";

  src = fetchFromGitHub {
    owner = "YOUR_GITHUB_OWNER";
    repo = pname;
    rev = "v${version}";
    hash = lib.fakeHash;
  };

  cargoHash = lib.fakeHash;
  nativeBuildInputs = [ makeWrapper ];
  doCheck = false; # Cannot have a nested container.
  postInstall = "install -D man/agentsandbox.1 $out/share/man/man1/agentsandbox.1";
  postFixup = ''
    wrapProgram "$out/bin/${pname}" \
      --prefix PATH : ${lib.makeBinPath [ libvirt openssh util-linux virtiofsd ]}
  '';
  meta = with lib; {
    description = "Manage isolated NixOS VM sandboxes for agentic workflows";
    homepage = "https://github.com/YOUR_GITHUB_OWNER/agentsandbox";
    license = lib.licenses.mit;
    mainProgram = pname;
    maintainers = with lib.maintainers; [ ownername ];
    platforms = [ "x86_64-linux" ];
  };
}
