# letsnote-wheelpad

> English: see [README.md](README.md).

Panasonic Let's Note の **ホイールパッド**（タッチパッド外周をなぞる円形スクロール）を Linux で再現するユーザランドデーモンです。Windows と同じく、タッチパッドの外周をゆっくり円を描くようにスワイプすると縦スクロールします。

物理 Synaptics タッチパッドの evdev イベントを直接読み取り、通常のポインター／マルチフィンガー入力用のミラータッチパッドと、円形スクロール用の仮想ホイールという 2 つの `uinput` デバイスを公開するため、Wayland でも X11 でも動作します。

## なぜこれが必要か

`libinput` は Wayland 時代に円形スクロールの追加を見送りました（2015 年 Peter Hutterer の議論を参照）。したがって、Let's Note の円形スクロールを Linux で動かす唯一の方法は、evdev を介してタッチパッドを直接読み、別の仮想デバイスからホイールイベントを発行するユーザランドデーモンを実装することです。本プロジェクトはまさにそれです。

## インストール

### Ubuntu / Debian

```sh
sudo dpkg -i letsnote-wheelpad_0.1.0_amd64.deb
systemctl --user enable --now letsnote-wheelpad.service
```

### Fedora / RHEL

```sh
sudo rpm -i letsnote-wheelpad-0.1.0-1.x86_64.rpm
systemctl --user enable --now letsnote-wheelpad.service
```

### Arch

```sh
yay -S letsnote-wheelpad      # AUR
systemctl --user enable --now letsnote-wheelpad.service
```

### ソースから

```sh
git clone https://github.com/Nerahikada/letsnote-wheelpad
cd letsnote-wheelpad
cargo build --release
sudo install -Dm755 target/release/letsnote-wheelpad /usr/bin/letsnote-wheelpad
sudo install -Dm644 packaging/udev/70-letsnote-wheelpad.rules /etc/udev/rules.d/70-letsnote-wheelpad.rules
sudo install -Dm644 packaging/systemd/letsnote-wheelpad.service /etc/systemd/user/letsnote-wheelpad.service
sudo install -Dm644 packaging/modules-load/letsnote-wheelpad.conf /etc/modules-load.d/letsnote-wheelpad.conf
sudo udevadm control --reload-rules && sudo udevadm trigger
sudo modprobe uinput
systemctl --user daemon-reload
systemctl --user enable --now letsnote-wheelpad.service
```

## 設定

設定ファイルは `~/.config/letsnote-wheelpad/config.toml` です。すべてのキーは省略可能で、デフォルト値は Windows の出荷時設定と一致します。

```toml
# 通常は名前正規表現で自動検出される。手動指定は非標準のパッドのみ。
# device = "/dev/input/event4"
# device_name_regex = "Synaptics.*TM3562"

[scroll]
enable               = true   # マスター有効
reverse_vertical     = false  # 縦スクロール方向を反転
horizontal_enable    = false  # 下端ウェッジでの横スクロールを有効化
reverse_horizontal   = false
sensitivity          = 0      # -2..+2 ; 小さいほど低感度
detect_area_width    = 0      # 0..10 ; 0=外周のみ, 10=全面
horizontal_start     = 2      # 円弧開始位置 (π/8 単位 ; 2 → 45°)
horizontal_end       = 6      # 円弧終了位置 (π/8 単位 ; 6 → 135°)

[log]
level = "info"  # trace | debug | info | warn | error
```

| キー | デフォルト | 範囲 | 備考 |
| --- | --- | --- | --- |
| `scroll.enable` | `true` | bool | 無効化するとデーモンは起動したまま全スクロールを抑制。 |
| `scroll.reverse_vertical` | `false` | bool | "ナチュラルスクロール" は `true`。 |
| `scroll.horizontal_enable` | `false` | bool | Windows と同じく出荷時 OFF。 |
| `scroll.reverse_horizontal` | `false` | bool | |
| `scroll.sensitivity` | `0` | -2..+2 | 倍率テーブル `[10, 14, 20, 28, 40]` のインデックス。 |
| `scroll.detect_area_width` | `0` | 0..10 | `0`=外周のみ、`10`=全面でスクロール開始可能。 |
| `scroll.horizontal_start` | `2` | 0..15 | π/8 単位。45°→135° のデフォルトはパッド下端。 |
| `scroll.horizontal_end` | `6` | 0..15 | |

### ログを見る

```sh
journalctl --user -u letsnote-wheelpad -f
```

スクロール感度がおかしいときは、設定ファイルの `scroll.sensitivity`（-2..+2）で調整してください。本デーモンは自動キャリブレーションを行いません — 履歴容量は Windows 完全互換のため 20 スロット固定です（DECISIONS.md D-021-followup 参照）。

## 既知の制限・非対応事項

- **`WheelUnderCursor` は設定不可。** Wayland ではコンポジタがフォーカス先サーフェスにイベントを配るため、ユーザランドからの上書きはできません。
- **テスト対象は Synaptics TM3562-3 系列のみ。** 他のタッチパッドでも `device_name_regex` を変更すれば動く可能性はありますが、動作保証はしません。
- **Excel 用矢印キーフォールバックは削除。** 現代の Excel は横ホイールイベントをネイティブで処理するため、Windows 版のハックは不要です。
- **コースティング/慣性スクロールなし。** Windows 版 WheelPad に合わせています。xf86 にはありますが、本プロジェクトでは実装しません。

## マルチフィンガージェスチャの優先順位

円形スクロールは 1 本指ジェスチャです。円形スクロールが確定する前は、通常のマルチフィンガージェスチャを優先します。

- 外周での 1 本指ジェスチャが候補の間は、完全な `SYN_REPORT` フレームを即座に転送せず、一時的に保持します。この間に 2 本目の指が触れると、保持したフレームを変更せずに再生して透過専用の `MultiTouch` 状態へ入ります。その後は全ての指が離れるまで円形認識を無効にし、2 本目だけを離しても同じジェスチャの途中では再び有効にしません。
- 円形スクロールの確定には、少なくとも 3 個の有効なサンプル、7.5° の角移動、半径方向より優勢な接線方向の移動、および移動方向と一致する正味の曲率が必要です。7.5° を超えても証拠が足りない場合は、ジェスチャを永久に拒否せず候補のまま観察を続けるため、密なサンプリング、ノイズ、ゆっくり形成される円弧にも対応できます。明確な半径方向の移動、持続する接線方向の直線、または有効サンプル 20 個の観察上限に達した場合は通常のポインター入力と判定します。その際は保持したフレームを変更せずに再生し、全指リリースまで `Passthrough` を維持します。さらに、ほとんど有効な移動を含まないレポートに対しては、別の生フレーム 64 個の上限でバッファを制限します。
- 円形スクロールが確定すると、保持したフレームを破棄するため、libinput は捕捉した接触を一度も認識せず、開始時のカーソルの飛びも発生しません。確定後に追加された指も公開せず、検出器は最初の tracking ID だけを追跡します。最初の指が先に離れても、別の指を円形軌跡へ継ぎ足しません。
- 捕捉した接触は仮想タッチパッド上で開始されていないため、最後の指離しも抑止できます。MT スロットや接触キーの終了イベントを合成しなくても、libinput に古い接触を残しません。

要するに、円形スクロールの確定前はマルチフィンガー入力が優先され、確定済みの円形ジェスチャは全指リリースまで所有権を維持します。次の接触シーケンスでは改めて判定を開始します。

## 仕組み（一段落版）

起動時に物理タッチパッドを `EVIOCGRAB` で恒久的に占有し、libinput がアタッチする 2 つの仮想 `uinput` デバイス（物理パッド能力をミラーリングしたタッチパッドと、ホイール）を生成します。7 状態の FSM（`Idle`、`Contact`、`Moving`、`MultiTouch`、`Passthrough`、`Scrolling`、`Debounce`）が入力を調停します。通常のイベントはそのまま転送しますが、外周で始まる 1 本指の候補だけは例外で、`Moving` が初期軌跡を判定する間、完全なフレームを保持します。ポインターまたはマルチフィンガーと判定すれば順番どおり再生し、円形と判定すれば破棄して、その物理接触ストリームの残りも抑止します。円形検出器には候補サンプルを事前投入し、隣接サンプル間の方向ベクトル角を積分して、±π を超えるたびに仮想ホイールから 1 ノッチを発行します。捕捉した接触は仮想タッチパッドへ一度も届かないため、開始時のカーソルの飛びも、全指リリース時に libinput へ残る古い接触状態も発生しません。

アルゴリズムの詳細とアーキテクチャの変更経緯は `DECISIONS.md` を参照してください（パススルー化の決定は D-022、アルゴリズム選択は D-008〜D-021）。

## ライセンス

MIT。[LICENSE](LICENSE) を参照。

## 謝辞

- Panasonic — オリジナルの WheelPad 設計者。
- X.Org `xf86-input-synaptics` プロジェクト — リバースエンジニアリング時の比較対象となった「中心からの角度」リファレンス実装。
- Peter Hutterer — [2015 年の libinput 議論](https://gitlab.freedesktop.org/libinput/libinput/-/issues/)。これが libinput パッチではなくデーモンとして実装すべき理由を明らかにしてくれました。
