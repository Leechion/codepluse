// Token usage dashboard: aggregates the per-turn usage captured from the
// adapters' session/prompt replies (see acp.normalizeUsage) over the persisted
// conversation turns. Contract mirror of codeg-ui/src/lib/types.ts
// TokenUsageReport / TokenUsageFacets / TokenUsageSyncStatus.
package codeg

import (
	"fmt"
	"sort"
	"time"
)

const usageScanCap = 50000

type usageFact struct {
	convID     int
	folderID   int
	agentType  string
	model      string
	at         time.Time
	in         float64
	out        float64
	cw         float64
	cr         float64
	durationMs float64
}

func (f usageFact) total() float64 { return f.in + f.out + f.cw + f.cr }

// collectUsageFacts snapshots the usage-bearing assistant turns, oldest
// first, capped at the most recent usageScanCap rows (truncated reports that).
// Conversation metadata (folder, agent, model, timestamps) is joined while the
// store lock is held.
func (s *Server) collectUsageFacts() ([]usageFact, bool) {
	st := s.store
	st.mu.Lock()
	defer st.mu.Unlock()
	facts := []usageFact{}
	for key, rows := range st.Turns {
		convID := atoiSafe(key)
		var conv Row
		if convID > 0 {
			conv = st.conversationLocked(convID)
		}
		folderID, agentType := 0, ""
		if conv != nil {
			folderID = number(conv["folder_id"])
			agentType = text(conv["agent_type"])
		}
		for _, t := range rows {
			if text(t["role"]) != "assistant" {
				continue
			}
			usage, ok := t["usage"].(map[string]any)
			if !ok || len(usage) == 0 {
				continue
			}
			at, err := time.Parse(time.RFC3339, text(t["timestamp"]))
			if err != nil {
				continue
			}
			fact := usageFact{
				convID:    convID,
				folderID:  folderID,
				agentType: agentType,
				model:     text(t["model"]),
				at:        at,
				in:        f64(usage["input_tokens"]),
				out:       f64(usage["output_tokens"]),
				cw:        f64(usage["cache_creation_input_tokens"]),
				cr:        f64(usage["cache_read_input_tokens"]),
			}
			if dm, ok := t["duration_ms"].(float64); ok {
				fact.durationMs = dm
			}
			facts = append(facts, fact)
		}
	}
	sort.Slice(facts, func(i, j int) bool { return facts[i].at.Before(facts[j].at) })
	truncated := false
	if len(facts) > usageScanCap {
		facts = facts[len(facts)-usageScanCap:]
		truncated = true
	}
	return facts, truncated
}

func f64(v any) float64 {
	switch n := v.(type) {
	case int:
		return float64(n)
	case int64:
		return float64(n)
	case float64:
		return n
	}
	return 0
}

// usageZone builds the viewer's local zone; the UI sends the JS offset
// convention (`-new Date().getTimezoneOffset()`), so UTC+8 arrives as 480.
func usageZone(minutes float64) *time.Location {
	if minutes == 0 {
		return time.UTC
	}
	return time.FixedZone("viewer", int(minutes)*60)
}

func weekdayMonday(t time.Time) int { return (int(t.Weekday()) + 6) % 7 }

func dayStart(t time.Time) time.Time {
	return time.Date(t.Year(), t.Month(), t.Day(), 0, 0, 0, 0, t.Location())
}

// usageBucketKey buckets a timestamp for the viewer's zone and returns the
// bucket key plus its [start, end) span.
func usageBucketKey(at time.Time, bucket string) (string, time.Time, time.Time) {
	lt := at.In(at.Location())
	switch bucket {
	case "month":
		start := time.Date(lt.Year(), lt.Month(), 1, 0, 0, 0, 0, lt.Location())
		return start.Format("2006-01"), start, start.AddDate(0, 1, 0)
	case "week":
		start := dayStart(lt).AddDate(0, 0, -weekdayMonday(lt))
		return start.Format("2006-01-02"), start, start.AddDate(0, 0, 7)
	default:
		start := dayStart(lt)
		return start.Format("2006-01-02"), start, start.AddDate(0, 0, 1)
	}
}

type usageWindow struct {
	facts []usageFact
	start time.Time
	end   time.Time
}

// applyUsageFilter keeps facts inside [start,end) matching folder / agent /
// model selections. Folder ids expand to their descendant folders so a
// project row covers its worktree children.
func (s *Server) applyUsageFilter(facts []usageFact, folders map[string]any, start, end time.Time, expandFolders func([]int) map[int]bool) usageWindow {
	agentSel := map[string]bool{}
	for _, a := range toStrSliceAny(folders["agentTypes"]) {
		agentSel[a] = true
	}
	modelSel := map[string]bool{}
	for _, m := range toStrSliceAny(folders["models"]) {
		modelSel[m] = true
	}
	folderSel := map[int]bool{}
	for _, v := range toNumSliceAny(folders["folderIds"]) {
		folderSel[v] = true
	}
	if len(folderSel) > 0 && expandFolders != nil {
		folderSel = expandFolders(keysOf(folderSel))
	}
	out := make([]usageFact, 0, len(facts))
	for _, f := range facts {
		if !start.IsZero() && f.at.Before(start) {
			continue
		}
		if !end.IsZero() && !f.at.Before(end) {
			continue
		}
		if len(agentSel) > 0 && !agentSel[f.agentType] {
			continue
		}
		if len(modelSel) > 0 && !modelSel[f.model] {
			continue
		}
		if len(folderSel) > 0 && !folderSel[f.folderID] {
			continue
		}
		out = append(out, f)
	}
	return usageWindow{facts: out, start: start, end: end}
}

func keysOf(m map[int]bool) []int {
	out := make([]int, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	return out
}

func toStrSliceAny(v any) []string {
	raw, ok := v.([]any)
	if !ok {
		return nil
	}
	out := make([]string, 0, len(raw))
	for _, item := range raw {
		if s, ok := item.(string); ok {
			out = append(out, s)
		}
	}
	return out
}

func toNumSliceAny(v any) []int {
	raw, ok := v.([]any)
	if !ok {
		return nil
	}
	out := make([]int, 0, len(raw))
	for _, item := range raw {
		if f, ok := item.(float64); ok {
			out = append(out, int(f))
		}
	}
	return out
}

type usageSum struct {
	in, out, cw, cr float64
	durationMs      float64
	turns           int
	convs           map[int]bool
	days            map[string]bool
}

func (u *usageSum) add(f usageFact, zone *time.Location) {
	u.in += f.in
	u.out += f.out
	u.cw += f.cw
	u.cr += f.cr
	u.durationMs += f.durationMs
	u.turns++
	u.convs[f.convID] = true
	u.days[f.at.In(zone).Format("2006-01-02")] = true
}

func (u *usageSum) totalsRow(zone *time.Location) Row {
	total := u.in + u.out + u.cw + u.cr
	return Row{"input_tokens": u.in, "output_tokens": u.out,
		"cache_creation_tokens": u.cw, "cache_read_tokens": u.cr,
		"total_tokens": total, "turn_count": u.turns,
		"conversation_count": len(u.convs), "duration_ms": u.durationMs,
		"active_days": len(u.days)}
}

// expandFoldersLocked returns the selection plus every descendant folder.
func (s *Server) expandFoldersLocked(ids []int) map[int]bool {
	st := s.store
	st.mu.Lock()
	defer st.mu.Unlock()
	children := map[int][]int{}
	for _, f := range st.Folders {
		if p := number(f["parent_id"]); p > 0 {
			children[p] = append(children[p], number(f["id"]))
		}
	}
	out := map[int]bool{}
	var walk func(int)
	walk = func(id int) {
		if out[id] {
			return
		}
		out[id] = true
		for _, c := range children[id] {
			walk(c)
		}
	}
	for _, id := range ids {
		walk(id)
	}
	return out
}

func (s *Server) tokenUsageReport(a Row) (any, error) {
	filter, _ := a["filter"].(map[string]any)
	if filter == nil {
		filter = Row{}
	}
	zone := usageZone(f64(filter["tzOffsetMinutes"]))
	bucket, _ := filter["bucket"].(string)
	if bucket != "day" && bucket != "week" && bucket != "month" {
		bucket = "day"
	}
	var start, end time.Time
	if v := text(filter["start"]); v != "" {
		start, _ = time.Parse(time.RFC3339, v)
	}
	if v := text(filter["end"]); v != "" {
		end, _ = time.Parse(time.RFC3339, v)
	}

	facts, truncated := s.collectUsageFacts()
	win := s.applyUsageFilter(facts, filter, start, end, s.expandFoldersLocked)

	var prev usageWindow
	compare := filter["comparePrevious"] == true && !start.IsZero() && !end.IsZero()
	if compare {
		span := end.Sub(start)
		prev = s.applyUsageFilter(facts, filter, start.Add(-span), start, s.expandFoldersLocked)
	}

	sum := usageSum{convs: map[int]bool{}, days: map[string]bool{}}
	series := []Row{}
	seriesIdx := map[string]int{}
	byFolder := map[string]*usageSum{}
	byAgent := map[string]*usageSum{}
	byModel := map[string]*usageSum{}
	heat := map[[2]int]*usageSum{}
	perConv := map[int]*usageSum{}
	convMeta := map[int]usageFact{}

	folderNames := map[int]Row{}
	st := s.store
	st.mu.Lock()
	for _, f := range st.Folders {
		folderNames[number(f["id"])] = f
	}
	convRows := map[int]Row{}
	for _, c := range st.Conversations {
		convRows[number(c["id"])] = c
	}
	st.mu.Unlock()

	folderLabel := func(id int) any {
		f := folderNames[id]
		if f == nil {
			return nil
		}
		if alias := text(f["alias"]); alias != "" {
			return alias
		}
		return text(f["name"])
	}

	for _, f := range win.facts {
		sum.add(f, zone)
		key, bStart, bEnd := usageBucketKey(f.at.In(zone), bucket)
		idx, ok := seriesIdx[key]
		if !ok {
			idx = len(series)
			seriesIdx[key] = idx
			series = append(series, Row{"bucket_key": key, "start": bStart.Format(time.RFC3339),
				"end": bEnd.Format(time.RFC3339), "input_tokens": 0.0, "output_tokens": 0.0,
				"cache_creation_tokens": 0.0, "cache_read_tokens": 0.0, "total_tokens": 0.0,
				"turn_count": 0, "conversation_count": 0})
		}
		point := series[idx]
		point["input_tokens"] = point["input_tokens"].(float64) + f.in
		point["output_tokens"] = point["output_tokens"].(float64) + f.out
		point["cache_creation_tokens"] = point["cache_creation_tokens"].(float64) + f.cw
		point["cache_read_tokens"] = point["cache_read_tokens"].(float64) + f.cr
		point["total_tokens"] = point["total_tokens"].(float64) + f.total()
		point["turn_count"] = point["turn_count"].(int) + 1

		// conversation_count per bucket needs a distinct set; keep one map per
		// bucket outside the row.
		convMeta[f.convID] = f
		if perConv[f.convID] == nil {
			perConv[f.convID] = &usageSum{convs: map[int]bool{}, days: map[string]bool{}}
		}
		perConv[f.convID].add(f, zone)

		addSum := func(m map[string]*usageSum, key string) *usageSum {
			if m[key] == nil {
				m[key] = &usageSum{convs: map[int]bool{}, days: map[string]bool{}}
			}
			return m[key]
		}
		bf := addSum(byFolder, fmt.Sprint(f.folderID))
		bf.add(f, zone)
		ba := addSum(byAgent, f.agentType)
		ba.add(f, zone)
		if f.model != "" {
			bm := addSum(byModel, f.model)
			bm.add(f, zone)
		}
		lt := f.at.In(zone)
		cell := [2]int{weekdayMonday(lt), lt.Hour()}
		if heat[cell] == nil {
			heat[cell] = &usageSum{convs: map[int]bool{}, days: map[string]bool{}}
		}
		heat[cell].add(f, zone)
	}

	// Distinct conversation counts per bucket.
	bucketConvs := map[string]map[int]bool{}
	for _, f := range win.facts {
		key, _, _ := usageBucketKey(f.at.In(zone), bucket)
		if bucketConvs[key] == nil {
			bucketConvs[key] = map[int]bool{}
		}
		bucketConvs[key][f.convID] = true
	}
	for key, convs := range bucketConvs {
		if idx, ok := seriesIdx[key]; ok {
			series[idx]["conversation_count"] = len(convs)
		}
	}

	breakdown := func(m map[string]*usageSum, label func(string) any) []Row {
		out := []Row{}
		for key, sum := range m {
			total := sum.in + sum.out + sum.cw + sum.cr
			out = append(out, Row{"key": key, "label": label(key),
				"input_tokens": sum.in, "output_tokens": sum.out,
				"cache_creation_tokens": sum.cw, "cache_read_tokens": sum.cr,
				"total_tokens": total, "turn_count": sum.turns,
				"conversation_count": len(sum.convs)})
		}
		sort.Slice(out, func(i, j int) bool {
			return out[i]["total_tokens"].(float64) > out[j]["total_tokens"].(float64)
		})
		return out
	}

	heatmap := []Row{}
	for wd := 0; wd < 7; wd++ {
		for h := 0; h < 24; h++ {
			cell := Row{"weekday": wd, "hour": h, "total_tokens": 0.0, "turn_count": 0}
			if sum := heat[[2]int{wd, h}]; sum != nil {
				cell["total_tokens"] = sum.in + sum.out + sum.cw + sum.cr
				cell["turn_count"] = sum.turns
			}
			heatmap = append(heatmap, cell)
		}
	}

	top := []Row{}
	convIDs := make([]int, 0, len(perConv))
	for id := range perConv {
		convIDs = append(convIDs, id)
	}
	sort.Slice(convIDs, func(i, j int) bool {
		ti := perConv[convIDs[i]].in + perConv[convIDs[i]].out + perConv[convIDs[i]].cw + perConv[convIDs[i]].cr
		tj := perConv[convIDs[j]].in + perConv[convIDs[j]].out + perConv[convIDs[j]].cw + perConv[convIDs[j]].cr
		return ti > tj
	})
	for _, id := range convIDs {
		if len(top) >= 5 {
			break
		}
		sum := perConv[id]
		last := ""
		if m := convMeta[id]; m.convID == id {
			last = m.at.Format(time.RFC3339)
		}
		title, agent := any(nil), ""
		folderLabelValue := any(nil)
		if c := convRows[id]; c != nil {
			if t := text(c["title"]); t != "" {
				title = t
			}
			agent = text(c["agent_type"])
		}
		if f := convMeta[id]; f.convID == id {
			folderLabelValue = folderLabel(f.folderID)
		}
		top = append(top, Row{"conversation_id": id, "title": title, "agent_type": agent,
			"folder_label": folderLabelValue,
			"total_tokens": sum.in + sum.out + sum.cw + sum.cr, "turn_count": sum.turns,
			"last_activity_at": last})
	}

	streak := usageStreakRow(win.facts, zone)

	var first, last string
	if len(win.facts) > 0 {
		first = win.facts[0].at.Format(time.RFC3339)
		last = win.facts[len(win.facts)-1].at.Format(time.RFC3339)
	}
	var rangeStart, rangeEnd any
	if !win.start.IsZero() {
		rangeStart = win.start.Format(time.RFC3339)
	}
	if !win.end.IsZero() {
		rangeEnd = win.end.Format(time.RFC3339)
	}
	var prevRow any
	if compare {
		pSum := usageSum{convs: map[int]bool{}, days: map[string]bool{}}
		for _, f := range prev.facts {
			pSum.add(f, zone)
		}
		prevRow = pSum.totalsRow(zone)
	}

	return Row{"range_start": rangeStart, "range_end": rangeEnd, "bucket": bucket,
		"totals": sum.totalsRow(zone), "previous_totals": prevRow,
		"series": series, "by_folder": breakdown(byFolder, func(key string) any {
			id := atoiSafe(key)
			if l := folderLabel(id); l != nil {
				return l
			}
			return key
		}), "by_agent": breakdown(byAgent, func(key string) any { return key }),
		"by_model": breakdown(byModel, func(key string) any { return key }),
		"heatmap":  heatmap, "top_conversations": top, "streak": streak,
		"first_activity_at": firstOrNil(first), "last_activity_at": firstOrNil(last),
		"truncated": truncated}, nil
}

func atoiSafe(s string) int {
	n := 0
	for _, r := range s {
		if r < '0' || r > '9' {
			return 0
		}
		n = n*10 + int(r-'0')
	}
	return n
}

func firstOrNil(s string) any {
	if s == "" {
		return nil
	}
	return s
}

// usageStreakRow computes the longest run of consecutive active days and the
// run that reaches back from the most recent active day, both in the viewer's
// local dates.
func usageStreakRow(facts []usageFact, zone *time.Location) Row {
	dates := map[string]bool{}
	for _, f := range facts {
		dates[f.at.In(zone).Format("2006-01-02")] = true
	}
	keys := make([]string, 0, len(dates))
	for d := range dates {
		keys = append(keys, d)
	}
	sort.Strings(keys)
	longest, current := 0, 0
	runStart := 0
	for i, d := range keys {
		if i == 0 || nextDay(keys[i-1]) != d {
			runStart = i
		}
		if i-runStart+1 > longest {
			longest = i - runStart + 1
		}
	}
	if len(keys) > 0 {
		current = 1
		for i := len(keys) - 1; i > 0; i-- {
			if nextDay(keys[i-1]) == keys[i] {
				current++
			} else {
				break
			}
		}
	}
	var endsOn any
	if len(keys) > 0 {
		endsOn = keys[len(keys)-1]
	}
	return Row{"longest_days": longest, "current_days": current, "current_ends_on": endsOn}
}

func nextDay(day string) string {
	t, err := time.Parse("2006-01-02", day)
	if err != nil {
		return ""
	}
	return t.AddDate(0, 0, 1).Format("2006-01-02")
}

func (s *Server) tokenUsageFacets() (any, error) {
	facts, _ := s.collectUsageFacts()
	agents := map[string]bool{}
	models := map[string]bool{}
	var dataStart, dataEnd string
	for _, f := range facts {
		if f.agentType != "" {
			agents[f.agentType] = true
		}
		if f.model != "" {
			models[f.model] = true
		}
	}
	agentList := mapKeys(agents)
	modelList := mapKeys(models)
	sort.Strings(agentList)
	sort.Strings(modelList)
	st := s.store
	st.mu.Lock()
	folders := []Row{}
	for _, f := range st.Folders {
		alias := text(f["alias"])
		name := text(f["name"])
		label := name
		if alias != "" {
			label = alias
		}
		folders = append(folders, Row{"folder_id": number(f["id"]), "label": label,
			"name": name, "alias": aliasOrNil(alias), "path": text(f["path"]),
			"parent_id": parentOrNil(f["parent_id"])})
	}
	st.mu.Unlock()
	if len(facts) > 0 {
		dataStart = facts[0].at.Format(time.RFC3339)
		dataEnd = facts[len(facts)-1].at.Format(time.RFC3339)
	}
	return Row{"folders": folders, "agents": agentList, "models": modelList,
		"data_start": firstOrNil(dataStart), "data_end": firstOrNil(dataEnd)}, nil
}

func mapKeys(m map[string]bool) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	return out
}

func aliasOrNil(alias string) any {
	if alias == "" {
		return nil
	}
	return alias
}

func parentOrNil(v any) any {
	if n := number(v); n > 0 {
		return n
	}
	return nil
}

func (s *Server) tokenUsageStatus() (any, error) {
	facts, _ := s.collectUsageFacts()
	convs := map[int]bool{}
	for _, f := range facts {
		convs[f.convID] = true
	}
	total := 0
	st := s.store
	st.mu.Lock()
	total = len(st.Conversations)
	st.mu.Unlock()
	var lastSync any
	if len(facts) > 0 {
		lastSync = facts[len(facts)-1].at.Format(time.RFC3339)
	}
	return Row{"total_conversations": total, "synced_conversations": len(convs),
		"stale_conversations": 0, "fact_rows": len(facts),
		"last_synced_at": lastSync, "running": false}, nil
}

// tokenUsageSync re-reads the persisted turns; the Go runtime keeps no separate
// facts store, so both modes complete immediately.
func (s *Server) tokenUsageSync(a Row) (any, error) {
	facts, _ := s.collectUsageFacts()
	total := 0
	st := s.store
	st.mu.Lock()
	for _, rows := range st.Turns {
		for _, t := range rows {
			if text(t["role"]) == "assistant" {
				total++
			}
		}
	}
	st.mu.Unlock()
	tokens := 0.0
	for _, f := range facts {
		tokens += f.total()
	}
	return Row{"scanned": total, "synced": len(facts), "skipped": total - len(facts),
		"failed": 0, "lost": 0, "turns_written": 0, "tokens_written": tokens,
		"pruned_conversations": 0}, nil
}
