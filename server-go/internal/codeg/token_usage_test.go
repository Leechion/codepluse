package codeg

import (
	"testing"
)

// seedUsage writes folders, conversations and usage-bearing assistant turns
// straight into the store so aggregate tests are deterministic.
func seedUsage(t *testing.T, s *Server) {
	t.Helper()
	st := s.store
	st.mu.Lock()
	defer st.mu.Unlock()
	st.Folders = append(st.Folders,
		Row{"id": 1, "name": "proj", "path": "/p", "parent_id": nil, "alias": nil, "kind": "regular"},
		Row{"id": 2, "name": "wt", "path": "/p/wt", "parent_id": 1, "alias": nil, "kind": "regular"})
	st.Conversations = append(st.Conversations,
		Row{"id": 11, "folder_id": 1, "agent_type": "claude_code", "title": "主对话", "status": "pending_review", "kind": "regular", "updated_at": "2026-03-03T02:00:00Z"},
		Row{"id": 12, "folder_id": 2, "agent_type": "codex", "title": "子任务", "status": "pending_review", "kind": "delegate", "updated_at": "2026-03-04T02:00:00Z"})
	usage := func(in, out, cw, cr float64) Row {
		return Row{"input_tokens": in, "output_tokens": out, "cache_creation_input_tokens": cw, "cache_read_input_tokens": cr}
	}
	mk := func(conv int, at, model string, u Row, dur float64) Row {
		return Row{"id": id(), "role": "assistant", "blocks": []Row{}, "timestamp": at,
			"usage": u, "model": model, "duration_ms": dur}
	}
	if err := st.replaceTurnsLocked(11, []Row{
		mk(11, "2026-03-02T01:00:00Z", "glm-5.3-flash", usage(100, 20, 10, 30), 1500),
		mk(11, "2026-03-03T01:30:00Z", "glm-5.3-flash", usage(200, 40, 20, 60), 2500),
	}); err != nil {
		t.Fatal(err)
	}
	if err := st.replaceTurnsLocked(12, []Row{
		mk(12, "2026-03-04T03:00:00Z", "deepseek-flash", usage(50, 5, 0, 10), 800),
		mk(12, "2026-03-05T03:00:00Z", "deepseek-flash", usage(70, 7, 0, 14), 900),
	}); err != nil {
		t.Fatal(err)
	}
}

func TestTokenUsageReportTotalsAndSeries(t *testing.T) {
	s := testServer(t)
	seedUsage(t, s)

	report, err := s.tokenUsageReport(Row{"filter": Row{"bucket": "day", "tzOffsetMinutes": 0}})
	if err != nil {
		t.Fatal(err)
	}
	r := report.(Row)
	totals := r["totals"].(Row)
	// 4 turns: in 420, out 72, cw 30, cr 114 → total 636.
	if got := totals["total_tokens"].(float64); got != 636 {
		t.Fatalf("total_tokens 应为 636，实际 %v", got)
	}
	if got := totals["turn_count"].(int); got != 4 {
		t.Fatalf("turn_count 应为 4，实际 %v", got)
	}
	if got := totals["conversation_count"].(int); got != 2 {
		t.Fatalf("conversation_count 应为 2，实际 %v", got)
	}
	if got := totals["active_days"].(int); got != 4 {
		t.Fatalf("active_days 应为 4，实际 %v", got)
	}
	series := r["series"].([]Row)
	if len(series) != 4 {
		t.Fatalf("day 桶应有 4 个点，实际 %d", len(series))
	}
	if first := series[0]["bucket_key"].(string); first != "2026-03-02" {
		t.Fatalf("首个桶应为 2026-03-02，实际 %s", first)
	}
	byAgent := r["by_agent"].([]Row)
	if len(byAgent) != 2 || byAgent[0]["key"].(string) != "claude_code" {
		t.Fatalf("by_agent 应按用量降序且 claude_code 居首，实际 %v", byAgent)
	}
	byModel := r["by_model"].([]Row)
	if len(byModel) != 2 || byModel[0]["key"].(string) != "glm-5.3-flash" {
		t.Fatalf("by_model 应以 glm-5.3-flash 居首，实际 %v", byModel)
	}
	heatmap := r["heatmap"].([]Row)
	if len(heatmap) != 168 {
		t.Fatalf("heatmap 应为 168 格，实际 %d", len(heatmap))
	}
	top := r["top_conversations"].([]Row)
	if len(top) != 2 || top[0]["conversation_id"].(int) != 11 {
		t.Fatalf("top_conversations 应以会话 11 居首，实际 %v", top)
	}
	streak := r["streak"].(Row)
	if streak["longest_days"].(int) != 4 || streak["current_days"].(int) != 4 {
		t.Fatalf("streak 应为 longest=4 current=4，实际 %v", streak)
	}
	if r["previous_totals"] != nil {
		t.Fatal("未开启 comparePrevious 时 previous_totals 应为 nil")
	}
}

func TestTokenUsageReportFiltersAndPrevious(t *testing.T) {
	s := testServer(t)
	seedUsage(t, s)

	// Agent filter narrows to the codex conversation.
	report, err := s.tokenUsageReport(Row{"filter": Row{"bucket": "day", "tzOffsetMinutes": 0, "agentTypes": []any{"codex"}}})
	if err != nil {
		t.Fatal(err)
	}
	totals := report.(Row)["totals"].(Row)
	if totals["turn_count"].(int) != 2 || totals["total_tokens"].(float64) != 156 {
		t.Fatalf("codex 过滤后应为 2 turns/156 tokens，实际 %v", totals)
	}

	// Time window keeps only 2026-03-02.
	report, err = s.tokenUsageReport(Row{"filter": Row{"bucket": "day", "tzOffsetMinutes": 0,
		"start": "2026-03-02T00:00:00Z", "end": "2026-03-03T00:00:00Z", "comparePrevious": true}})
	if err != nil {
		t.Fatal(err)
	}
	r := report.(Row)
	if got := r["totals"].(Row)["total_tokens"].(float64); got != 160 {
		t.Fatalf("窗口内应为 160 tokens，实际 %v", got)
	}
	prev, ok := r["previous_totals"].(Row)
	if !ok || prev == nil {
		t.Fatal("comparePrevious 应返回 previous_totals")
	}
	// Previous window is 2026-03-01 → 2026-03-02; no turn falls inside it.
	if prev["turn_count"].(int) != 0 {
		t.Fatalf("前窗应为空（0 turn），实际 %v", prev)
	}

	// Folder expansion: selecting folder 1 must include its worktree child 2.
	report, err = s.tokenUsageReport(Row{"filter": Row{"bucket": "day", "tzOffsetMinutes": 0, "folderIds": []any{float64(1)}}})
	if err != nil {
		t.Fatal(err)
	}
	if got := report.(Row)["totals"].(Row)["turn_count"].(int); got != 4 {
		t.Fatalf("folder 展开后应含全部 4 turns，实际 %v", got)
	}

	// Viewer timezone -480 (UTC-8) shifts the 01:00Z turn into the previous day.
	report, err = s.tokenUsageReport(Row{"filter": Row{"bucket": "day", "tzOffsetMinutes": -480}})
	if err != nil {
		t.Fatal(err)
	}
	series := report.(Row)["series"].([]Row)
	found := false
	for _, p := range series {
		if p["bucket_key"].(string) == "2026-03-01" {
			found = true
		}
	}
	if !found {
		t.Fatalf("tz=-480 时 2026-03-02T01:00Z 应落入 2026-03-01 桶，实际 %v", series)
	}
}

func TestTokenUsageFacetsStatusSync(t *testing.T) {
	s := testServer(t)
	seedUsage(t, s)

	facets, err := s.tokenUsageFacets()
	if err != nil {
		t.Fatal(err)
	}
	f := facets.(Row)
	if len(f["folders"].([]Row)) != 2 {
		t.Fatalf("facets 应含 2 个文件夹，实际 %v", f["folders"])
	}
	if len(f["agents"].([]string)) != 2 {
		t.Fatalf("facets 应含 2 个执行端，实际 %v", f["agents"])
	}
	if len(f["models"].([]string)) != 2 {
		t.Fatalf("facets 应含 2 个模型，实际 %v", f["models"])
	}
	if f["data_start"].(string) != "2026-03-02T01:00:00Z" {
		t.Fatalf("data_start 应为最早 turn，实际 %v", f["data_start"])
	}

	status, err := s.tokenUsageStatus()
	if err != nil {
		t.Fatal(err)
	}
	st := status.(Row)
	if st["fact_rows"].(int) != 4 || st["synced_conversations"].(int) != 2 || st["running"] != false {
		t.Fatalf("status 异常: %v", st)
	}

	sync, err := s.tokenUsageSync(Row{"mode": "incremental"})
	if err != nil {
		t.Fatal(err)
	}
	res := sync.(Row)
	if res["synced"].(int) != 4 || res["failed"].(int) != 0 {
		t.Fatalf("sync 结果异常: %v", res)
	}
	if res["tokens_written"].(float64) != 636 {
		t.Fatalf("tokens_written 应为 636，实际 %v", res["tokens_written"])
	}
}
