# Rhlk

X68000 向けリンカ **HLK/HLKX** を、Rust でホスト OS ネイティブ動作させるために移植したプロジェクトです。  
`Rhlk/` 配下に Rust 実装本体があります。

## このリポジトリについて
- オリジナル実装: HLK / HLKX（68000 アセンブリ実装）
- 本実装: Rust による移植版（`rhlk`）
- 目的: 既存 HLKX 互換動作を、開発しやすい形で再現すること

## 免責・注意事項
- 本プロジェクトは **AI を用いた実験的移植** です。
- Rust 実装コードは **人手で直接コーディングせず、AI 生成のみ** で構築しています。
- **無保証** です。利用によって生じたいかなる損害についても責任を負いません。
- 実運用では必ず、対象ワークロードで追加検証してください。

## 検証状況（概要）
以下の検証を継続実施しています。

- Rust unit/integration tests
  - `cargo test -q --manifest-path Rhlk/Cargo.toml`
- オリジナル HLKX との回帰比較（run68x 利用）
  - `./tools/run_hlkx_regression.sh`
  - 標準出力/標準エラーは統合比較
  - 生成物（実行ファイルや中間出力）も比較
- 最終生成物比較
  - HAS + 原版 HLK + Rhlk の end-to-end 比較を実施

詳細な手順・結果は以下を参照してください。
- `docs/porting/2026-02-27-test-guide.md`
- `docs/porting/2026-02-27-regression-harness.md`
- `docs/porting/2026-02-27-final-executable-compare.md`

## ライセンスについて
- HLK は **そると氏** の著作物です。
- HLKX 改造部分は **TcbnErik 氏** の著作物です。
- 本リポジトリの Rust 移植部分は別実装ですが、オリジナル由来部分の権利関係を変更するものではありません。
- オリジナル配布条件・ライセンスの詳細は必ず `external/hlkx/README.md` を参照してください。

## 関連
- HLKX: `external/hlkx`
- run68x: `external/run68x`
- u8tosj: `external/u8tosj`
