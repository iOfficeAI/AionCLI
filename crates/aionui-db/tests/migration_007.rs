//! Migration 007 integration test: builtin ACP agent rows are rewritten to
//! `npm exec` with `${AGENT_PREFIX}` / `${AGENT_NPM_CACHE}` placeholders.

use aionui_db::init_database_memory;
use sqlx::Row;

#[tokio::test]
async fn migration_007_rewrites_three_builtin_agents() {
    let db = init_database_memory().await.expect("init memory db");
    let pool = db.pool();

    let rows = sqlx::query(
        "SELECT id, command, args,
                json_extract(agent_source_info, '$.binary_name') AS binary_name,
                json_extract(agent_source_info, '$.bridge_binary') AS bridge_binary
         FROM agent_metadata
         WHERE agent_source = 'builtin'
           AND json_extract(agent_source_info, '$.binary_name') IN ('claude','codex','codebuddy')",
    )
    .fetch_all(pool)
    .await
    .expect("query");

    assert_eq!(rows.len(), 3, "expected three builtin rows");

    for row in &rows {
        let binary: String = row.get("binary_name");
        let command: String = row.get("command");
        let args: String = row.get("args");
        let bridge: String = row.get("bridge_binary");

        assert_eq!(command, "npm", "{binary}: command must be npm");
        assert_eq!(bridge, "npm", "{binary}: bridge_binary must be npm");
        assert!(
            args.contains("\"exec\""),
            "{binary}: args must use `exec` subcommand: {args}"
        );
        assert!(
            args.contains("--prefix=${AGENT_PREFIX}"),
            "{binary}: args must reference AGENT_PREFIX placeholder: {args}"
        );
        assert!(
            args.contains("--cache=${AGENT_NPM_CACHE}"),
            "{binary}: args must reference AGENT_NPM_CACHE placeholder: {args}"
        );
    }

    let claude = rows
        .iter()
        .find(|r| r.get::<String, _>("binary_name") == "claude")
        .unwrap();
    let claude_args: String = claude.get("args");
    assert!(
        claude_args.contains("@agentclientprotocol/claude-agent-acp@"),
        "claude args must reference the claude-agent-acp package: {claude_args}"
    );

    let codex = rows
        .iter()
        .find(|r| r.get::<String, _>("binary_name") == "codex")
        .unwrap();
    let codex_args: String = codex.get("args");
    assert!(
        codex_args.contains("@zed-industries/codex-acp@"),
        "codex args must reference codex-acp package: {codex_args}"
    );

    let codebuddy = rows
        .iter()
        .find(|r| r.get::<String, _>("binary_name") == "codebuddy")
        .unwrap();
    let codebuddy_args: String = codebuddy.get("args");
    assert!(
        codebuddy_args.contains("@tencent-ai/codebuddy-code@"),
        "codebuddy args must reference codebuddy-code: {codebuddy_args}"
    );
    assert!(
        codebuddy_args.contains("\"--acp\""),
        "codebuddy args must keep the `--acp` flag: {codebuddy_args}"
    );
}
