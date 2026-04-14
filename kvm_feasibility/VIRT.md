# VIRT

## 概要

`virt.sh` は `qemu:///session` を使って、単発の diskless QEMU/KVM 仮想マシンを起動する。
`sandbox.sh` 互換は捨て、`~/.local/sandbox` 配下に state を置く。

この variant の必須要件は次の通り。

- booted `systemd`
- nested virtualization
- disk image を持たない
- user 権限で起動できる

## 方針

- hypervisor は `qemu:///session`
- state root は `~/.local/sandbox`
- `subuid` / `subgid` は使わない
- guest filesystem 共有は `virtiofs`
- networking は `interface type='user'` + `backend type='passt'`
- direct kernel boot を使う
- guest は booted `systemd` を PID 1 に持つ
- nested virtualization を guest に見せる

`sandbox.sh` と state や ownership contract は共有しない。

## 目標

- 1 台だけ単発起動できる
- `systemd`, `journalctl`, `libvirtd`, `virsh` が guest 内で使える
- nested virtualization が guest 内で機能する
- root filesystem は揮発である
- host 側 state は `~/.local/sandbox` に閉じる
- `ssh -p 2223 vscode@127.0.0.1` で接続できる
- 必要なら VFIO GPU を guest に渡せる

## 非目標

- `sandbox.sh` 互換
- `/sandbox` 配下 state の再利用
- `subuid` ベースの host/guest ownership isolation
- 複数 VM 同時起動
- TCG fallback
- 9p fallback
- libvirt への永続 define

## Host 前提

- recent `x86_64` Linux
- user session libvirt が使える
- `virsh -c qemu:///session uri` が成功する
- current user から `/dev/kvm` を読める
- host nested KVM が有効
- host port `2223` が未使用

必要コマンドは以下。

- `virsh`
- `qemu-system-x86_64`
- `virtiofsd`
- `ssh`
- `ss`

VFIO GPU を使う場合だけ、追加で以下を前提にする。

- 対象 GPU の IOMMU group 全体が boot 時から `vfio-pci` に bind 済み
- current user が `/dev/vfio/vfio` と `/dev/vfio/<group>` を読書きできる

## state layout

state root は `~/.local/sandbox` とする。

- `~/.local/sandbox/virt/kernel`
- `~/.local/sandbox/virt/initrd`
- `~/.local/sandbox/virt/kernel-params`
- `~/.local/sandbox/virt/domain.xml`
- `~/.local/sandbox/virt/serial.log`
- `~/.local/sandbox/virt/lock`
- `~/.local/sandbox/virt/rootfs/`
- `~/.local/sandbox/virt/persistent/`
- `~/.local/sandbox/virt/workspace/`

この配下に qcow2/raw disk image を置いてはならない。

## ownership 方針

`subuid` は使わない。
共有ディレクトリの ownership は host 実 UID/GID を正とする。

- guest の `vscode` UID は host 実 UID と一致させる
- guest の `vscode` GID は host 実 GID と一致させる
- `virtiofs` は `idmap` を使わない
- shared path 上では guest root ではなく `vscode` で作業する前提にする

これにより `~/.local/sandbox/virt/persistent` と
`~/.local/sandbox/virt/workspace` は user ownership のまま扱える。

## CLI

```
usage: ./virt.sh [help|build|up|down|kill|pause|unpause|exec|logs|ssh|console|add|delete|mount|unmount|lsmount|--] [args ...]
```

- `build`
  guest 用 NixOS variant と boot artifact を更新する
- `up`
  build 後に transient VM を起動し、serial console に attach する
- `down`
  `virsh -c qemu:///session shutdown agenthouse-virt`
- `kill`
  `virsh -c qemu:///session destroy agenthouse-virt`
- `pause`
  `virsh -c qemu:///session suspend agenthouse-virt`
- `unpause`
  `virsh -c qemu:///session resume agenthouse-virt`
- `exec`
  `ssh` 経由で command を実行する
- `logs`
  guest 内 `journalctl` を実行する
- `ssh`
  `ssh -p 2223 vscode@127.0.0.1`
- `console`
  `virsh -c qemu:///session console agenthouse-virt`

subcommand なしの挙動は以下。

- VM が起動中なら `console`
- VM が停止中なら `build` してから `up`
- `-- cmd ...` は `exec -- cmd ...` と同義

## Build 仕様

`virt.sh build` は guest 専用 variant `agenthouse-virt-session` を build する。

build artifact の契約は次の通り。

- `result/kernel`
- `result/initrd`
- `result/kernel-params`
- `result/sw`

`virt.sh` はこれらを `~/.local/sandbox/virt/` に symlink し直す。

## guest variant

guest は NixOS variant とし、`systemd` を boot する。
container variant ではない。

必須要件は以下。

- `boot.isContainer = false`
- `boot.loader.grub.enable = false`
- `boot.loader.systemd-boot.enable = false`
- `boot.initrd.systemd.enable = true`
- serial console login 有効
- `services.openssh.enable = true`
- `services.qemuGuest.enable = true`
- guest 内 `virtualisation.libvirtd.enable = true`
- guest 内 `programs.virt-manager.enable = false`
- guest 内 `environment.systemPackages` に `qemu_kvm`, `virsh`, `jq`, `tmux`
- `users.users.vscode.uid = host_uid`
- `users.users.vscode.extraGroups = [ "wheel" "libvirtd" "kvm" ]`

## root filesystem

root filesystem は tmpfs とする。
永続化は user-owned な shared path に閉じる。

- `/` は tmpfs
- `/nix` は read-only `virtiofs`
- `/persistent` は read-write `virtiofs`
- `/workspace` は read-write `virtiofs`

`/home/vscode` は `/persistent/home/vscode` を使う。
guest の永続 state は原則ここに寄せる。

## kernel cmdline

最低限以下を含める。

- `console=ttyS0`
- `systemd.journald.forward_to_console=1`
- `systemd.unit=multi-user.target`

## libvirt domain

domain 名は `agenthouse-virt` 固定とする。
起動方式は transient domain で、`virsh create` を使う。

必須条件は以下。

- connection は `qemu:///session`
- hypervisor type は `kvm`
- machine type は `q35`
- direct kernel boot
- disk device なし
- `on_poweroff=destroy`
- `on_reboot=restart`
- `on_crash=destroy`
- `memoryBacking` は shared `memfd`
- network は `passt`
- SSH は host `127.0.0.1:2223` から guest `22`
- graphics は `none`

最低限の XML イメージは以下。

```xml
<domain type='kvm'>
  <name>agenthouse-virt</name>
  <os>
    <type arch='x86_64' machine='q35'>hvm</type>
    <kernel>/home/USER/.local/sandbox/virt/kernel</kernel>
    <initrd>/home/USER/.local/sandbox/virt/initrd</initrd>
    <cmdline>console=ttyS0 ...</cmdline>
  </os>
  <cpu mode='host-passthrough' migratable='off'/>
  <memoryBacking>
    <source type='memfd'/>
    <access mode='shared'/>
  </memoryBacking>
  <devices>
    <filesystem type='mount' accessmode='passthrough'>
      <driver type='virtiofs'/>
      <binary path='/home/USER/.nix-profile/bin/virtiofsd'/>
      <source dir='/home/USER/.local/sandbox/virt/rootfs/nix'/>
      <target dir='nix'/>
      <readonly/>
    </filesystem>
    <filesystem type='mount' accessmode='passthrough'>
      <driver type='virtiofs'/>
      <binary path='/home/USER/.nix-profile/bin/virtiofsd'/>
      <source dir='/home/USER/.local/sandbox/virt/persistent'/>
      <target dir='persistent'/>
    </filesystem>
    <filesystem type='mount' accessmode='passthrough'>
      <driver type='virtiofs'/>
      <binary path='/home/USER/.nix-profile/bin/virtiofsd'/>
      <source dir='/home/USER/.local/sandbox/virt/workspace'/>
      <target dir='workspace'/>
    </filesystem>
    <interface type='user'>
      <backend type='passt'/>
      <model type='virtio'/>
      <portForward proto='tcp'>
        <range start='2223' to='22'/>
      </portForward>
    </interface>
    <channel type='unix'>
      <target type='virtio' name='org.qemu.guest_agent.0'/>
    </channel>
    <serial type='file'>
      <source path='/home/USER/.local/sandbox/virt/serial.log'/>
    </serial>
    <console type='pty'/>
  </devices>
</domain>
```

## nested virtualization

nested は optional ではない。

host 側では以下を満たすこと。

- `/dev/kvm` が current user から使える
- `kvm_intel` または `kvm_amd` の nested が有効
- domain CPU は `host-passthrough`

guest 側では以下を満たすこと。

- `vmx` または `svm` flag が見える
- `/dev/kvm` が存在する
- `systemctl is-active libvirtd` が success
- `vscode` が guest 内で `virsh -c qemu:///system list` を実行できる

## VFIO GPU

VFIO GPU は optional とする。
使う場合の前提は厳密に固定する。

- GPU は boot 時から `vfio-pci` に bind 済み
- 同じ IOMMU group の全 function も `vfio-pci` に bind 済み
- current user が `/dev/vfio/vfio` と該当 group device を使える

この条件では XML は `managed='no'` を使う。
`managed='yes'` は採らない。

理由は以下。

- 起動時 detach は不要
- 停止時 reattach も不要
- `qemu:///session` に privileged device lifecycle を期待しない

hostdev の例は以下。

```xml
<hostdev mode='subsystem' type='pci' managed='no'>
  <driver name='vfio'/>
  <source>
    <address domain='0x0000' bus='0x03' slot='0x00' function='0x0'/>
  </source>
</hostdev>
```

## 起動シーケンス

`virt.sh up` は次の順で実行する。

1. `virsh -c qemu:///session uri` を確認する
2. `/dev/kvm` へのアクセスを確認する
3. nested 有効を確認する
4. port `2223` の空きを確認する
5. `build` を完了する
6. domain XML を書く
7. `virsh -c qemu:///session create` を実行する
8. console attach と readiness probe を並行で走らせる

readiness probe は SSH 優先で行う。

1. `ssh -p 2223 vscode@127.0.0.1 true` を retry する
2. `systemctl is-system-running --wait` を確認する
3. `systemctl is-active sshd libvirtd` を確認する
4. `grep -E '(vmx|svm)' /proc/cpuinfo` を確認する
5. `test -e /dev/kvm` を確認する

どれか 1 つでも失敗したら `up` は失敗扱いにする。

## セキュリティ前提

これは `qemu:///session` なので、QEMU は current user と同じ権限で動く。
`qemu:///system` のような root 管理 plane は持たない。

一方で shared path は user-owned なので、guest がそれらに書く操作は host user 資産に直結する。
`sandbox.sh` のような subuid-based isolation は提供しない。

VFIO GPU を使う場合は、その IOMMU group 全体を user に委譲することになる。
これは単なる render node 共有より強い権限である。
