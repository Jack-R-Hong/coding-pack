# plugin-coding-pack

Pulse coding plugin pack — AI 驅動的軟體開發工作流系統。

透過 BMAD 方法論的 AI 團隊（架構師、開發者、QA 等）與 Claude Code 協作，自動完成從設計到實作、測試、Code Review 的完整開發流程。

## 前置需求

- Rust 1.85+
- [Pulse](https://github.com/pulsate-labs/pulse) CLI 已安裝
- Anthropic API key（供 Claude Code 使用）

## Plugin 依賴

### Pack plugins（需建置安裝）

| Plugin | Repo |
|--------|------|
| `bmad-method` | Jack-R-Hong/bmad-method |
| `provider-claude-code` | Jack-R-Hong/provider-claude-code |
| `plugin-git-ops` | Jack-R-Hong/pulse-plugins-git-ops |
| `plugin-git-worktree` | Jack-R-Hong/plugin-git-worktree |
| `plugin-memory` | _(此 repo 內的 script)_ |

### Bridge plugins（runtime 委派，選用）

| Plugin | Repo |
|--------|------|
| `plugin-board` | Jack-R-Hong/plugin-board |
| `plugin-auto-loop` | Jack-R-Hong/plugin-auto-loop |
| `plugin-issue-sync` | Jack-R-Hong/plugin-issue-sync |
| `plugin-test-runner` | Jack-R-Hong/plugin-test-runner |
| `plugin-feedback-loop` | Jack-R-Hong/plugin-feedback-loop |
| `plugin-trigger-cron` | Jack-R-Hong/plugin-trigger-cron |
| `plugin-workspace-tracker` | Jack-R-Hong/plugin-workspace-tracker |

詳細說明見 [docs/architecture.md#plugin-dependencies](docs/architecture.md)。

## 安裝

### 1. 建置所有 plugin

```bash
# 建置本 plugin（orchestrator）
cargo build --release

# 建置相依的 sibling plugins
for d in provider-claude-code git-ops git-worktree bmad-method; do
  (cd ../pulse-plugins/$d && cargo build --release)
done
```

### 2. 安裝 plugin binaries

```bash
DEST=config/plugins

cp target/release/plugin-coding-pack $DEST/
cp ../provider-claude-code/target/release/provider-claude-code $DEST/
cp ../git-ops/target/release/plugin-git-ops $DEST/
cp ../git-worktree/target/release/plugin-git-worktree $DEST/
cp ../bmad-method/target/release/bmad-method $DEST/
```

### 3. 驗證安裝

```bash
PULSE_DB_PATH=sqlite:pulse.db?mode=rwc \
  pulse registry validate --config ./config
```

## 使用方式

所有工作流透過 `pulse run` 執行，以 JSON 格式傳入需求描述：

```bash
export PULSE_DB_PATH=sqlite:pulse.db?mode=rwc
```

### 快速開發（最常用）

適合小功能、快速修改，3 步完成（規劃 → 實作 → commit）：

```bash
pulse run coding-quick-dev --config ./config \
  -i '{"input": "在 login endpoint 加上 input validation"}'
```

### 完整功能開發

適合較大功能，5 步流程（架構設計 → 建立 worktree → 實作 → QA review → commit）：

```bash
pulse run coding-feature-dev --config ./config \
  -i '{"input": "實作使用者通知系統，支援 email 和 in-app 兩種管道"}'
```

### Story 驅動開發

從 user story 出發，6 步流程（SM 準備 story → 架構設計 → worktree → 實作 → QA → commit）：

```bash
pulse run coding-story-dev --config ./config \
  -i '{"input": "身為使用者，我希望能匯出 CSV 報表以便離線分析"}'
```

### Bug 修復

4 步流程（根因分析 → 修復 → edge case review → commit）：

```bash
pulse run coding-bug-fix --config ./config \
  -i '{"input": "當 user_id 為 null 時 /api/profile 回傳 500 而非 404"}'
```

### 重構

4 步流程（規劃漸進式重構 → 執行 → 回歸驗證 → commit）：

```bash
pulse run coding-refactor --config ./config \
  -i '{"input": "將 UserService 的 database 操作抽成 Repository pattern"}'
```

### Code Review

3 步平行審查（對抗式審查 + edge case 審查 → 綜合報告）：

```bash
pulse run coding-review --config ./config \
  -i '{"target": "src/auth/"}'
```

## Bootstrap 工作流（自我演進）

這些工作流用於開發 plugin 自身：

```bash
# 開發單一 plugin
pulse run bootstrap-plugin --config ./config \
  -i '{"input": "為 validator 加上 step dependency 循環檢測"}'

# 完整自我演進循環（plan → implement → test → review → rebuild → install → validate → commit）
pulse run bootstrap-cycle --config ./config \
  -i '{"input": "重構 pack.rs 的錯誤處理，改用 thiserror"}'
```

## Auto-Dev（自動開發 Daemon）

`auto-dev` 系統會持續監控 board 上 `ready-for-dev` 的任務，自動挑選並執行對應工作流，無需人工介入。

### 1. 啟動 pulse-server

```bash
pulse-server --config ./config
```

### 2. 建立 Board 任務

透過 API 新增一個待開發任務：

```bash
curl -X POST http://localhost:8080/api/v1/tasks \
  -H "Content-Type: application/json" \
  -d '{
    "metadata": {
      "title": "在 login endpoint 加上 input validation",
      "description": "對 /api/login 的 email 與 password 欄位加上格式驗證，回傳統一的 400 錯誤格式",
      "status": "ready-for-dev",
      "labels": ["quick-dev"]
    }
  }'
```

> **labels** 決定使用哪個工作流：`quick-dev` → `coding-quick-dev`、`feature` → `coding-feature-dev`、`bug` → `coding-bug-fix`、`refactor` → `coding-refactor`。

### 3. 啟動 Auto-Dev Daemon

每 60 秒輪詢一次 board，自動執行可用任務：

```bash
./scripts/auto-dev-daemon.sh 60
```

Daemon 會將執行紀錄輸出至 stdout，並在任務完成後自動將狀態更新為 `done`。

### 4. 手動觸發單次工作流

不經由 daemon，直接對指定工作流發送執行請求：

```bash
curl -X POST http://localhost:8080/api/v1/workflows/coding-quick-dev/execute \
  -H "Content-Type: application/json" \
  -d '{"input": "在 login endpoint 加上 input validation"}'
```

---

## Plugin 管理指令

透過 plugin 的 action API 查詢系統狀態：

```bash
# 檢查 pack 健康狀態
pulse exec plugin-coding-pack -i '{"action": "status"}'

# 驗證所有 plugin 是否就緒
pulse exec plugin-coding-pack -i '{"action": "validate-pack"}'

# 列出已註冊的工作流
pulse exec plugin-coding-pack -i '{"action": "list-workflows"}'

# 列出已安裝的 plugins
pulse exec plugin-coding-pack -i '{"action": "list-plugins"}'
```

## 設定

### config/config.yaml

```yaml
db_path: "pulse.db"       # SQLite 資料庫路徑
log_level: "info"          # 日誌等級：debug, info, warn, error
plugin_dir: "config/plugins"  # plugin binary 目錄
```

### 環境變數

| 變數 | 說明 | 範例 |
|------|------|------|
| `PULSE_DB_PATH` | SQLite 連線字串 | `sqlite:pulse.db?mode=rwc` |
| `PULSE_LLM_PROVIDER` | LLM 提供者（選填） | `anthropic` |
| `PULSE_LLM_MODEL` | 模型覆寫（選填） | `claude-sonnet-4-6` |

## 工作流一覽

| 工作流 | 步驟數 | 適用場景 |
|--------|--------|----------|
| `coding-quick-dev` | 3 | 小功能、快速修改 |
| `coding-feature-dev` | 5 | 完整功能開發 |
| `coding-story-dev` | 6 | 從 user story 出發的開發 |
| `coding-bug-fix` | 4 | Bug 修復與根因分析 |
| `coding-refactor` | 4 | 安全重構 |
| `coding-review` | 3 | 多層次 code review |
| `bootstrap-plugin` | 5 | 開發單一 plugin |
| `bootstrap-rebuild` | 3 | 重建所有 plugins |
| `bootstrap-cycle` | 8 | 完整自我演進循環 |

## AI 團隊成員

| 代號 | 名字 | 角色 |
|------|------|------|
| `bmad/architect` | Winston | 系統架構師 |
| `bmad/dev` | Amelia | 開發者 |
| `bmad/pm` | John | 產品經理 |
| `bmad/qa` | Quinn | QA 工程師 |
| `bmad/sm` | Bob | Scrum Master |
| `bmad/quick-flow-solo-dev` | Barry | 快速開發專家 |
| `bmad/analyst` | Mary | 商業分析師 |
| `bmad/ux-designer` | Sally | UX 設計師 |
| `bmad/tech-writer` | Paige | 技術寫手 |

## 開發

```bash
# 執行測試（16 tests）
cargo test

# 建置
cargo build --release
```

詳細技術文件見 [docs/plugin-coding-pack.md](docs/plugin-coding-pack.md)。

## License

MIT OR Apache-2.0
