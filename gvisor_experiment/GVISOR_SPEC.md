# GVISOR_SPEC

## 概要

`sandbox-gvisor.sh` は、このリポジトリの NixOS 設定を使って、
`runsc` で 1 個の gVisor sandbox を起動するランチャーである。

現行 `sandbox.sh` から残す契約は次の 3 つだけに絞る。

- guest 内で `systemd` を PID 1 として boot する
- host 側 workspace を起動中 sandbox に動的に追加・削除できる
- host kernel への露出を減らす方向で安全性を維持する

逆に、次は意図的に捨てる。

- `subuid` / `subgid` を使った ownership isolation
- host 固定 path `/sandbox`
- `nsenter` と pidfile による再接続
- host tty の mode 緩和
- host cgroup subtree の `chown`
- idmapped bind mount

この variant は `bubblewrap` 系の rootless container を延長しない。
`runsc` を OCI runtime として直接使う。

## 目標

- state root を `~/.local/state/agenthouse-gvisor` に寄せる
- guest PID 1 は `systemd` であり、起動 entrypoint は
  `/nix/var/nix/profiles/system/init`
- host `/workspace/<basename>` 相当の動的 mount を restart なしで反映する
- `runsc exec` だけで再接続できる
- runtime hotplug の undocumented API に依存しない
- 現行 `configuration.nix` の persistence contract を再利用する

## 非目標

- rootless `runsc` 対応
- host port `2222` への SSH forward
- network namespace isolation
- 複数 sandbox 同時起動
- Docker / containerd / Podman 依存
- host `/sys/fs/cgroup` を guest に bind すること
- guest root を host 実 UID から隔離すること

## rootless を採らない理由

gVisor の一次情報に従い、v1 は rootless を採らない。

- `runsc --rootless` は `create` 非対応で、主用途が `runsc do` に限られる
- 同方式では gVisor netstack が使えず、外部接続は host network 前提になる
- OCI spec での true rootless も multi-UID map には `newuidmap` 等が要る
- single-UID mapping では普通の Linux image の展開が壊れる

したがって、v1 は `sudo runsc` を受け入れ、その代わり state root は
user-owned な `~/.local/state/agenthouse-gvisor` に閉じる。

## 安全モデル

この variant の安全性は、subid ではなく gVisor 側の境界で担保する。

- sandboxed workload は host kernel に直接 system call しない
- host filesystem は OCI spec で渡した path だけが見える
- guest が触れる writable host path は `persistent` と登録済み workspace だけ
- final runtime の network は `host` をデフォルトにする
- final runtime では `--directfs=false` を指定する
- bind mount は `shared` 前提で使い、`exclusive` は使わない
- host `/sys`, host cgroup, host tty は bind しない

`subuid` を捨てるので、guest root が共有 path を root-owned で汚すこと自体は
防がない。これは仕様上の trade-off として受け入れる。
その代わり、通常作業ユーザー `vscode` の UID/GID は host 実 UID/GID に合わせる。

network も host 側をそのまま使うため、v1 は network 隔離を提供しない。
この点も明示的な trade-off とする。

## state layout

state root は固定で以下とする。

`~/.local/state/agenthouse-gvisor`

```text
~/.local/state/agenthouse-gvisor/
├── rootfs/                  # nixos/nix から展開した OCI rootfs, root-owned 可
├── persistent/              # guest /persistent, user-owned
│   └── workspace/           # 動的 mount の staging 先
├── bundle/
│   └── config.json          # 本番 sandbox 用 OCI spec
├── build/
│   └── config.json          # build helper 用 OCI spec
├── runsc/                   # runsc --root
├── log/
│   ├── runsc.boot.log
│   └── runsc.exec.log
├── mounts                   # 登録済み host path 一覧
└── local.nix                # host UID/GID を埋めた generated module
```

`/` 直下に state を作ってはならない。

## CLI

```text
usage: ./sandbox-gvisor.sh [help|build|up|down|kill|exec|logs|add|delete|mount|unmount|lsmount|--] [args ...]
```

- `help`
  usage を表示する
- `build`
  rootfs と OCI bundle を更新し、guest system を build する
- `up`
  build 後に sandbox を起動し、ready まで待つ
- `down`
  guest 内 `systemctl poweroff` で clean shutdown する
- `kill`
  `runsc kill` と `runsc delete` で強制停止する
- `exec`
  起動中 sandbox で command を実行する。引数なしなら shell
- `logs`
  guest 内 `journalctl` を実行する。default は `-en1000`
- `add`
  host path を registry に登録し、その場で host bind mount する
- `delete`
  host bind mount を外し、registry から削除する
- `mount`
  registry にある全 path を host bind mount する
- `unmount`
  registry にある全 path を host unmount する
- `lsmount`
  registry 内容を表示する

subcommand なしの挙動は次とする。

- sandbox が起動中なら `exec`
- 停止中なら `build` してから `up`
- `-- cmd ...` は `exec -- cmd ...` と同義

## 現行 `sandbox.sh` から引き継ぐ contract

- NixOS build の入力は repo 内 `flake.nix` と `configuration.nix`
- 永続化 root は guest 内 `/persistent`
- `/workspace` は impermanence が `/persistent/workspace` から生やす
- basename collision は現行と同様に reject する
- 1 インスタンスだけ起動できる

## build 仕様

`build` は 2 段階で行う。

1. rootfs bootstrap
2. NixOS system build

### 1. rootfs bootstrap

- `rootfs/` が未初期化なら `nixos/nix:latest` を Docker Hub から取る
- layer 展開は ownership を保つ必要があるため `sudo tar --same-owner` を使う
- 展開先は `rootfs/` で、`/sandbox` は使わない
- `rootfs/etc/nixos/` に以下を置く
  - `flake.nix`
  - `configuration.nix`
  - `local.nix`

### 2. NixOS system build

build は host に Nix を要求しない。
helper container も `runsc` で起動する。

- `build/config.json` は `runsc spec` から生成して最小差分だけ patch する
- helper command は `nix build /etc/nixos#nixosConfigurations.agenthouse-gvisor.config.system.build.toplevel`
- build helper は runtime と同じく host networking を使う
- build 結果は `rootfs/nix/var/nix/profiles/system` に out-link する
- `boot.isContainer = true` は維持する

## generated local module

`local.nix` は毎回 `build` で上書き生成する。
役割は host 実 UID/GID を guest の通常作業ユーザーに合わせることだけとする。

最低限の内容は以下。

- `users.users.vscode.uid = host_uid`
- `users.groups.vscode.gid = host_gid`
- `users.users.vscode.group = "vscode"`
- `users.users.vscode.extraGroups = [ "wheel" "systemd-journal" ]`

`agenthouse-gvisor` は既存 `agenthouse` と別 output にする。
現行 user 実装は変更しない。

## runtime bundle 仕様

本番 sandbox は `bundle/config.json` から起動する。

`config.json` の生成方針は次の通り。

- 手書き JSON を増やさず `runsc spec` を base にする
- patch 対象は `root.path`, `process.args`, `process.env`, `mounts`, `linux.namespaces` に絞る
- container id は固定で `agenthouse-gvisor`

### process

- PID 1 command は `/nix/var/nix/profiles/system/init`
- `TERM=xterm-256color` を入れる
- `container=oci` を入れる
- detached 起動を前提にする

### mounts

必須 mount は以下。

- `/proc`
- `/sys`
- `/sys/fs/cgroup`
- `/dev`
- `/dev/pts`
- `/dev/shm`
- `/run`
- `/tmp`
- `/persistent`

`/sys` と `/sys/fs/cgroup` は host bind ではなく、
sandbox 内の仮想 `sysfs` / `cgroup2` mount を使う。

`/persistent` mount は host `persistent/` を bind する。
ここだけ read-write とする。

`/persistent` mount option は次を必須にする。

- `rbind`
- `rw`
- `rprivate`
- `dcache=0`

dynamic mount の可視性を優先するため、`persistent` 側の dentry cache は切る。

### runtime flags

本番起動時の `runsc` flag は最低限次を満たす。

- `--root "$STATE/runsc"`
- `--network=host`
- `--directfs=false`
- `--file-access-mounts=shared`
- `--debug-log "$STATE/log/runsc.%COMMAND%.log"`

`--file-access-mounts=exclusive` は禁止する。
host 側で `persistent/workspace` を外から変更するためである。

## systemd boot 仕様

`up` は foreground attach ではなく detached 起動とし、その後 readiness を待つ。

起動順は以下で固定する。

1. `build`
2. `mount`
3. `runsc run --bundle "$STATE/bundle" --detach agenthouse-gvisor`
4. `runsc exec agenthouse-gvisor systemctl is-system-running --wait`

ready 判定は以下。

- `systemctl is-system-running --wait` が `running` または `degraded`
- `systemctl is-active systemd-journald` が success
- `ps -p 1 -o comm=` が `systemd`

`down` は signal 直打ちではなく guest 内 command で行う。

- `runsc exec agenthouse-gvisor systemctl poweroff`

これにより pidfile, `pgrep --ns`, `nsenter` を不要にする。

## dynamic mount 仕様

gVisor の公式 doc が runtime 動的 mount として明示しているのは EROFS だけである。
v1 は bind mount hotplug API に依存しない。

dynamic mount は次の方法で実現する。

- sandbox 起動時点で host `persistent/` 全体を guest `/persistent` に bind する
- host 側 `persistent/workspace/<basename>` に対して bind / umount を行う
- guest `/workspace` は既存 impermanence contract により `/persistent/workspace` を見る

つまり、runtime に新 mount を追加するのではなく、
既に公開済みの `persistent` tree の中身だけを更新する。

### registry

- registry file は `$STATE/mounts`
- 1 行 1 path
- 登録順ではなく `LC_ALL=C sort -u` で正規化する
- basename collision は reject する

### add

- 引数なしなら `$PWD`
- `realpath -e` で正規化する
- `sudo mount --bind "$src" "$STATE/persistent/workspace/$base"` を行う
- mount 済みなら重複登録しない
- sandbox が停止中でも成功してよい

### delete

- `realpath -m` で正規化する
- registry に存在するものだけを対象にする
- `sudo umount "$STATE/persistent/workspace/$base"` を行う
- registry から削除する

### mount / unmount

- `mount` は registry 全件に対して host bind を張る
- `unmount` は registry 全件を host から外す
- sandbox の起動有無には依存しない

### correctness

official filesystem doc に従い、bind mounts は shared mode で使う。
host 側の変更が revalidate される前提を使う。

これは runtime bind hotplug の保証ではなく、
「既に露出した bind mount tree の中の host 変更が見える」ことに依存する。
そのため v1 は `persistent` のみを dynamic mount の carrier に固定する。

## ownership 仕様

`subid` は使わないので、ownership の基本方針は単純にする。

- host `persistent/` は user-owned
- host mount source も user-owned を前提にする
- guest の通常作業ユーザー `vscode` は host 実 UID/GID と一致させる
- host bind mount に idmap は使わない

guest root が shared path に root-owned file を作ることは可能である。
この点は v1 の既知制約とし、通常操作は `vscode` で行う。

## 捨てる実装断片

gVisor 版では次を実装しない。

- `nssudo`
- `map_inner`
- `running_pid` の ns 検索
- `sandbox_exec` の `nsenter`
- host tty の `chmod o+rw`
- host cgroup subtree の `chown`
- `pasta`
- `ssh`
- idmapped bind mount

## 受け入れ条件

- host `/etc/subuid` `/etc/subgid` を参照しない
- host `/sandbox` を作らない
- `./sandbox-gvisor.sh up` で guest `systemd` が PID 1 で起動する
- `./sandbox-gvisor.sh exec -- systemctl is-system-running` が成功する
- `./sandbox-gvisor.sh add <path>` 後、restart なしで guest `/workspace/<basename>` が見える
- `./sandbox-gvisor.sh delete <path>` 後、guest から当該 path が消える
- host tty permission と host cgroup ownership を変更しない
- exposed host path が `rootfs`, `persistent`, 登録済み workspace 以外に増えない
- guest は host networking で通信できる

## 将来拡張

必要になった場合だけ、別仕様として検討する。

- isolated networking
- EROFS rootfs 化
- KVM platform
- observability 用 `runsc export-metrics`
