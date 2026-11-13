package codeg

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"unicode/utf8"
)

// Task splits are the main bot's proposal surface: the model calls
// propose_task_split, a card goes up on the connection, and the user's
// decision on that card fans the work out through the same runDelegation
// machinery a direct delegate_to_agent uses.
//
// The tool returns *immediately* — it does not park the model's turn on the
// user's answer. A permission request parks inside the agent→client request
// loop (the driver blocks on app.reply); a codeg-mcp companion call is the
// other way round: the tool_use was routed out-of-band to an HTTP POST, so
// blocking it freezes the agent's turn with no deadline, while the user may
// take arbitrarily long to read a card. So the contract is: the proposal
// returns right away telling the model to hand off, the card is the real
// decision point, and an un-answered card keeps its full task text in the
// next turn's context (splitContext) so a new turn can still act on it.
type taskSplit struct {
	ID     string `json:"split_id"`
	Parent string `json:"parent_connection_id"`
	Root   string `json:"root_connection_id"`
	// ParentToolUseID binds the split to the propose_task_split call it came
	// from. The model invents tool-call ids on some harnesses and a re-proposal
	// after an edit reuses them, so a card is keyed by the split id and only
	// claims the row when it is still unclaimed.
	ParentToolUseID string `json:"parent_tool_use_id,omitempty"`
	Tasks           []Row  `json:"tasks"`
	CreatedAt       string `json:"created_at"`
}

func (s *Server) splitFile() string {
	return filepath.Join(s.Root, ".data", "codeg-task-splits.json")
}

// nilIfEmpty reports an empty map as an untyped nil. clone() of a nil Row
// yields a typed-nil Row, which is not == nil once boxed in an interface —
// the snapshot contract is "no pending card" = the key reads as null, not
// "split_id": null (a client truthing the object would keep showing a card
// that was already answered).
func nilIfEmpty(v Row) any {
	if len(v) == 0 {
		return nil
	}
	return v
}

func (s *Server) saveSplitsLocked() error {
	rows := []Row{}
	for _, sp := range s.splits {
		rows = append(rows, Row{"split_id": sp.ID, "parent_connection_id": sp.Parent, "root_connection_id": sp.Root, "parent_tool_use_id": sp.ParentToolUseID, "tasks": sp.Tasks, "created_at": sp.CreatedAt})
	}
	return atomicWrite(s.splitFile(), rows)
}

// loadSplits restores pending cards after a restart. Children launched before
// the shutdown recover through loadDelegations; a proposal that never got an
// answer comes back exactly as it was left, still waiting.
func (s *Server) loadSplits() {
	data, err := os.ReadFile(s.splitFile())
	if err != nil {
		return
	}
	var rows []Row
	if json.Unmarshal(data, &rows) != nil {
		return
	}
	for _, row := range rows {
		sp := &taskSplit{ID: text(row["split_id"]), Parent: text(row["parent_connection_id"]), Root: text(row["root_connection_id"]), ParentToolUseID: text(row["parent_tool_use_id"]), CreatedAt: text(row["created_at"])}
		if sp.ID == "" || sp.Parent == "" {
			continue
		}
		if tasks, ok := row["tasks"].([]any); ok {
			for _, raw := range tasks {
				if task, ok := raw.(map[string]any); ok {
					sp.Tasks = append(sp.Tasks, Row(task))
				}
			}
		}
		s.splits[sp.ID] = sp
	}
}

func (s *Server) splitWire(sp *taskSplit) Row {
	return Row{"split_id": sp.ID, "parent_connection_id": sp.Parent, "root_connection_id": sp.Root, "parent_tool_use_id": sp.ParentToolUseID, "tasks": sp.Tasks, "created_at": sp.CreatedAt}
}

// shortenTitle truncates a prompt-derived title to at most n bytes, backing off
// to a rune boundary so a CJK title never splits into invalid UTF-8.
func shortenTitle(s string, n int) string {
	if len(s) <= n {
		return s
	}
	cut := n
	for cut > 0 && !utf8.RuneStart(s[cut]) {
		cut--
	}
	return s[:cut]
}

// isWriteTask reads a subtask prompt for signs it changes files. Deliberately
// coarse and biased toward "no": a false positive leaves the task on the
// proposing agent (the safe default), while a false negative on a read-only
// task only costs a slightly more expensive harness. Phrases that negate a
// change ("不要修改任何文件", "do not write") are stripped before matching —
// models label read-only tasks with exactly those denials, and matching them
// verbatim pinned every survey task back onto the proposer.
func isWriteTask(prompt string) bool {
	lower := strings.ToLower(prompt)
	// Negated verbs must not read as write signals, and cuts must land on
	// rune boundaries: each match deletes itself plus a fixed window of the
	// following RUNES (byte-slicing a CJK prompt at an arbitrary offset
	// would splice a needle together out of two half-runes).
	for _, neg := range []string{"不要", "不需", "无需", "不得", "禁止", "切勿", "别", "not ", "never ", "without ", "no ", "don't ", "doesn't ", "cannot "} {
		for {
			i := strings.Index(lower, neg)
			if i < 0 {
				break
			}
			end, skipped := i+len(neg), 0
			for skipped < 24 && end < len(lower) {
				_, size := utf8.DecodeRuneInString(lower[end:])
				end += size
				skipped++
			}
			lower = lower[:i] + lower[end:]
		}
	}
	for _, needle := range []string{"修复", "实现", "重构", "编辑", "创建文件", "写入", "修改", "fix ", "implement", "refactor", "edit", "write", "create", "modify"} {
		if strings.Contains(lower, needle) {
			return true
		}
	}
	return false
}

// splitAgentEnabled reports whether an agent-list row (from s.agents()) is a
// harness a subtask may run on: enabled in the registry, and like pick() an
// agent whose real tool is "" or "echo" (the test stub) is always runnable.
// A named base tool's installation is deliberately NOT re-checked from
// installed_version here: installedVersion() returns "local" for any unknown
// tool (and for the test registry's ids), so it cannot separate installed
// from missing; connect() remains the authority when the task actually
// launches, and its failure surfaces as a failed subtask row.
func splitAgentEnabled(c Row) bool {
	return c["available"] == true && c["enabled"] == true && text(c["agent_type"]) != ""
}

// baseWire maps a real tool onto the wire name the base agent answers under
// (wireAgent's table); instance rows already carry their own wire.
func baseWire(tool string) string {
	if strings.HasPrefix(tool, "account:") {
		return "account_" + strings.TrimPrefix(tool, "account:")
	}
	switch tool {
	case "claude":
		return "claude_code"
	case "codex":
		return "codex"
	case "dsh":
		return "deepseek"
	case "workbuddy":
		return "custom:workbuddy"
	}
	return ""
}

// sameExecutor reports whether a candidate row runs the same tool as the
// proposer: the identical wire (the base row itself) or an instance of the
// same real tool. A proposer whose wire matches no candidate row (an agent
// that left the registry) is never matched by accident.
func sameExecutor(c Row, parentAgentType string) bool {
	if parentAgentType == "" {
		return false
	}
	if text(c["agent_type"]) == parentAgentType {
		return true
	}
	return c["instance"] == true && baseWire(text(c["real_tool"])) == parentAgentType
}

// availableSplitAgent finds the candidate row for a wanted agent_type,
// mirroring pick(): the wire name first, then a registry agent whose ID
// equals the wanted string (a model may echo a list_agents entry back as
// "claude" even though the base row answers as "claude_code").
func availableSplitAgent(candidates []Row, agentType string) (Row, bool) {
	if c := findSplitAgent(candidates, agentType); c != nil {
		return c, true
	}
	if c := findSplitAgent(candidates, baseWire(agentType)); c != nil {
		return c, true
	}
	return nil, false
}

// findSplitAgent returns the live candidate row for a wire name (an instance
// wire, or a base tool's wireAgent name). Instances precede base rows in the
// registry, so the first hit is the most specific. nil means the target is
// unknown, disabled, or unrunnable.
func findSplitAgent(candidates []Row, agentType string) Row {
	if agentType == "" {
		return nil
	}
	for _, c := range candidates {
		if text(c["agent_type"]) == agentType && splitAgentEnabled(c) {
			return c
		}
	}
	return nil
}

func firstAvailableSplitAgent(candidates []Row) string {
	for _, c := range candidates {
		if splitAgentEnabled(c) && text(c["agent_type"]) != "" {
			return text(c["agent_type"])
		}
	}
	return ""
}

// autoAssignSplitTarget picks a harness for a subtask the model left
// unassigned, so the card always shows a concrete choice and "confirm" never
// dispatches a half-decided plan. Heuristic: a read-only task moves off the
// proposing agent onto a different harness when one exists (an instance of
// the same real tool counts — a different pinned provider/model is still a
// different executor, and spreading work across the named instances is the
// point of the feature); write-shaped tasks stay on the proposer. The user
// edits the chip before confirming if the guess is wrong — the card, not the
// model, owns the decision.
func autoAssignSplitTarget(prompt string, candidates []Row, parentAgentType string) string {
	if isWriteTask(prompt) {
		return parentAgentType
	}
	for _, c := range candidates {
		at := text(c["agent_type"])
		if at != "" && at != parentAgentType && splitAgentEnabled(c) {
			return at
		}
	}
	return parentAgentType
}

// resolveSplitTarget maps a wanted agent_type onto a live candidate row,
// returning (wire, row, ok). An empty or unknown wanted falls back to the
// proposer (its own instance row outranking the bare base row), then to the
// first runnable row. ok=false means nothing is runnable at all.
func resolveSplitTarget(wanted string, candidates []Row, parentAgentType string) (string, Row, bool) {
	if c, ok := availableSplitAgent(candidates, wanted); ok {
		return text(c["agent_type"]), c, true
	}
	for _, c := range candidates {
		if c["instance"] == true && splitAgentEnabled(c) && (text(c["real_tool"]) == wanted || text(c["agent_type"]) == parentAgentType) {
			return text(c["agent_type"]), c, true
		}
	}
	// The proposer's own executor is the intended last resort. An instance
	// row of the same real tool outranks the bare base row: staying on the
	// proposer should keep the pinned provider/model the user configured
	// when the proposal came from a plain wire name.
	for _, c := range candidates {
		if c["instance"] == true && sameExecutor(c, parentAgentType) && splitAgentEnabled(c) {
			return text(c["agent_type"]), c, true
		}
	}
	if parentAgentType != "" {
		if c := findSplitAgent(candidates, parentAgentType); c != nil {
			return text(c["agent_type"]), c, true
		}
	}
	if at := firstAvailableSplitAgent(candidates); at != "" {
		for _, c := range candidates {
			if text(c["agent_type"]) == at {
				return at, c, true
			}
		}
	}
	return "", nil, false
}

func resolveSplitModelTarget(wanted, model string, candidates []Row, parent string) (string, Row, bool) {
	if model != "" {
		matches := []Row{}
		for _, row := range candidates {
			if text(row["agent_type"]) == wanted {
				break
			}
			if row["instance"] == true && text(row["provider_id"]) != "" && text(row["default_model"]) == model && (text(row["real_tool"]) == wanted || baseWire(text(row["real_tool"])) == wanted) {
				matches = append(matches, row)
			}
		}
		if len(matches) == 1 {
			return text(matches[0]["agent_type"]), matches[0], true
		}
		if len(matches) > 1 {
			return "", nil, false
		}
	}
	wire, row, ok := resolveSplitTarget(wanted, candidates, parent)
	if ok && text(row["provider_id"]) != "" && model != "" && model != text(row["default_model"]) {
		return "", nil, false
	}
	return wire, row, ok
}

// claimSplitToolUse links a split card to the parent's propose_task_split tool
// row so the transcript shows the card under the call that produced it. Callers
// hold s.mu.
func claimSplitToolUse(parent *Connection, sp *taskSplit) {
	if parent.ToolClaims == nil {
		parent.ToolClaims = map[string]bool{}
	}
	for _, tool := range parent.Tools {
		toolID := text(tool["tool_call_id"])
		if toolID == "" || parent.ToolClaims[toolID] || internalToolName(text(tool["title"])) != "propose_task_split" {
			continue
		}
		var args Row
		if json.Unmarshal([]byte(text(tool["raw_input"])), &args) != nil {
			continue
		}
		if raw, _ := args["tasks"].([]any); len(raw) != len(sp.Tasks) {
			continue
		}
		sp.ParentToolUseID = toolID
		parent.ToolClaims[toolID] = true
		return
	}
}

// proposeTaskSplit validates a proposal from the main bot, auto-fills the
// targets the model left blank, publishes the card, and returns immediately.
func (s *Server) proposeTaskSplit(caller *Caller, a Row) (any, error) {
	if caller.Depth >= 4 {
		return nil, fmt.Errorf("委派深度达到 4 层上限")
	}
	rawTasks, _ := a["tasks"].([]any)
	if len(rawTasks) == 0 || len(rawTasks) > 8 {
		return nil, fmt.Errorf("tasks 需要 1–8 个子任务")
	}
	for _, raw := range rawTasks {
		item, _ := raw.(map[string]any)
		if item == nil {
			return nil, fmt.Errorf("tasks 的每一项必须是对象")
		}
		prompt := strings.TrimSpace(text(item["prompt"]))
		if prompt == "" {
			return nil, fmt.Errorf("每个子任务都需要完整 prompt")
		}
		if len(prompt) > 120000 {
			return nil, fmt.Errorf("子任务的 prompt 不能超过 120000 字节")
		}
	}
	s.mu.Lock()
	parent := s.connections[caller.ConnectionID]
	if parent == nil {
		s.mu.Unlock()
		return nil, fmt.Errorf("父会话已断开")
	}
	// One pending card per connection: a model that proposes again before the
	// user answered the first must not bury it under a second decision.
	for _, existing := range s.splits {
		if existing.Parent == caller.ConnectionID {
			s.mu.Unlock()
			return nil, fmt.Errorf("已有待确认的拆分提案（split %s），请等待用户处理", existing.ID)
		}
	}
	// The card's dropdown is built from the live registry: an agent uninstalled
	// between the outer check and here must not offer a target that fails.
	candidates := s.delegationAgents()
	parentAgentType := parent.AgentType
	sp := &taskSplit{ID: id(), Parent: caller.ConnectionID, Root: caller.RootID, CreatedAt: timestamp()}
	for i, raw := range rawTasks {
		item, _ := raw.(map[string]any) // validated above
		title := strings.TrimSpace(text(item["title"]))
		prompt := strings.TrimSpace(text(item["prompt"]))
		if title == "" {
			title = shortenTitle(prompt, 60)
		}
		wanted := strings.TrimSpace(text(item["agent_type"]))
		auto := wanted == ""
		if auto {
			wanted = autoAssignSplitTarget(prompt, candidates, parentAgentType)
		}
		model := strings.TrimSpace(text(item["model"]))
		at, row, ok := resolveSplitModelTarget(wanted, model, candidates, parentAgentType)
		if !ok {
			s.mu.Unlock()
			return nil, fmt.Errorf("没有可用的执行端，无法拆分任务")
		}
		wanted = at
		if model == "" {
			// Default to the target instance's pinned model so the chip shows
			// what will actually run, and a confirmed dispatch carries it
			// explicitly instead of racing the connect-time default.
			model = text(row["default_model"])
		}
		sp.Tasks = append(sp.Tasks, Row{"index": i, "title": title, "prompt": prompt, "agent_type": wanted, "model": model, "auto": auto})
	}
	claimSplitToolUse(parent, sp)
	parent.PendingSplit = s.splitWire(sp)
	s.splits[sp.ID] = sp
	err := s.saveSplitsLocked()
	if err != nil {
		s.clearSplitLocked(sp)
		s.mu.Unlock()
		return nil, err
	}
	s.mu.Unlock()
	s.event(caller.ConnectionID, Row{"type": "task_split_request", "split": s.splitWire(sp)})
	return Row{"status": "proposed", "split_id": sp.ID,
		"note": "拆分提案已在用户界面展示：用户确认后系统会自动派发全部子任务，你无需（也禁止）自行调用 delegate_to_agent 重复派发。本轮到此结束即可；若用户发来新消息，先查看上方任务状态，切勿重复提案。"}, nil
}

// splitContext lists the splits waiting on a connection so a NEW turn (the
// proposal turn already returned) still knows what the user was asked to
// confirm. Mirrors delegationContext: read-only guidance, not a plan to
// re-execute.
func (s *Server) splitContext(cid string) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	var b strings.Builder
	for _, sp := range s.splits {
		if sp.Parent != cid {
			continue
		}
		fmt.Fprintf(&b, "拆分 %s：\n", sp.ID)
		for _, task := range sp.Tasks {
			fmt.Fprintf(&b, "- %s；执行端 %s；模型 %s\n", text(task["title"]), text(task["agent_type"]), text(task["model"]))
		}
	}
	if b.Len() == 0 {
		return ""
	}
	return "等待用户在界面上确认的拆分提案（确认后系统自动派发；不要自行重新提案或重复派发）：\n" + b.String() + "\n"
}

// respondTaskSplit applies the user's decision on the card. approve (the
// default) fans the confirmed task list out as child delegations and resolves
// the split; reject resolves it without launching. Either way the card is
// retired on every client through the same event pair that raised it, and the
// pending slot is cleared so a reload no longer offers a spent decision.
func (s *Server) respondTaskSplit(a Row) (any, error) {
	splitID := text(a["splitId"])
	action := text(a["action"])
	if action == "" {
		action = "approve"
	}
	s.mu.Lock()
	sp := s.splits[splitID]
	if sp == nil {
		s.mu.Unlock()
		return nil, fmt.Errorf("拆分提案不存在或已处理")
	}
	parent := s.connections[sp.Parent]
	if parent == nil {
		delete(s.splits, splitID)
		_ = s.saveSplitsLocked()
		s.mu.Unlock()
		return nil, fmt.Errorf("父会话已断开")
	}
	// The card is the editor of the plan: its confirmed task list replaces the
	// model's draft. An answer with no list (reject, or approve as-proposed)
	// keeps the proposal.
	if raw, ok := a["tasks"].([]any); ok && action == "approve" {
		if len(raw) == 0 || len(raw) > 8 {
			s.mu.Unlock()
			return nil, fmt.Errorf("tasks 需要 1–8 个子任务")
		}
		candidates := s.delegationAgents()
		tasks := []Row{}
		for i, rawTask := range raw {
			item, _ := rawTask.(map[string]any)
			if item == nil {
				s.mu.Unlock()
				return nil, fmt.Errorf("第 %d 个确认子任务无效", i+1)
			}
			prompt := strings.TrimSpace(text(item["prompt"]))
			if prompt == "" || len(prompt) > 120000 {
				s.mu.Unlock()
				return nil, fmt.Errorf("第 %d 个子任务的 prompt 必须非空且不超过 120000 字节", i+1)
			}
			title := strings.TrimSpace(text(item["title"]))
			if title == "" {
				title = shortenTitle(prompt, 60)
			}
			model := strings.TrimSpace(text(item["model"]))
			at, row, ok := resolveSplitModelTarget(text(item["agent_type"]), model, candidates, parent.AgentType)
			if !ok {
				s.mu.Unlock()
				return nil, fmt.Errorf("没有可用的执行端")
			}
			agentType := at
			if model == "" {
				model = text(row["default_model"])
			}
			tasks = append(tasks, Row{"index": i, "title": title, "prompt": prompt, "agent_type": agentType, "model": model})
		}
		sp.Tasks = tasks
	}
	// Take the card out of the registry first, under one lock, so a second
	// respond (double click, two windows) cannot launch the plan twice.
	s.clearSplitLocked(sp)
	if err := s.saveSplitsLocked(); err != nil {
		s.splits[sp.ID] = sp
		s.mu.Unlock()
		return nil, err
	}
	if action == "reject" {
		s.mu.Unlock()
		s.event(sp.Parent, Row{"type": "task_split_resolved", "split_id": splitID, "action": "reject"})
		return Row{"split_id": splitID, "action": "reject"}, nil
	}

	// Approve: hand the plan to the delegation fan-out. The split is already
	// retired, so a double click or a second window can only launch it once.
	parentDepth, parentConv, parentAgentType := parent.Depth, parent.ConversationID, parent.AgentType
	s.mu.Unlock()
	ids := s.launchDelegations(sp, parentDepth, parentConv, parentAgentType)
	if ids == nil {
		return nil, fmt.Errorf("拆分任务持久化失败")
	}
	s.event(sp.Parent, Row{"type": "task_split_resolved", "split_id": splitID, "action": "approve", "task_ids": ids})
	return Row{"split_id": splitID, "action": "approve", "task_ids": ids}, nil
}

// clearSplitLocked retires a handled split: gone from the registry, off the
// parent's snapshot slot, and un-claimed on its tool row. Callers hold s.mu.
func (s *Server) clearSplitLocked(sp *taskSplit) {
	delete(s.splits, sp.ID)
	if current := s.connections[sp.Parent]; current != nil {
		if current.PendingSplit != nil && text(current.PendingSplit["split_id"]) == sp.ID {
			current.PendingSplit = nil
		}
		if sp.ParentToolUseID != "" {
			delete(current.ToolClaims, sp.ParentToolUseID)
		}
	}
}

// dropConnectionSplits releases splits parked on a connection that is going
// away or ending its turn, so a card can never be answered after the session
// behind it is gone.
func (s *Server) dropConnectionSplits(cid string) {
	s.mu.Lock()
	dropped := []*taskSplit{}
	for _, sp := range s.splits {
		if sp.Parent == cid {
			s.clearSplitLocked(sp)
			dropped = append(dropped, sp)
		}
	}
	if len(dropped) > 0 {
		_ = s.saveSplitsLocked()
	}
	s.mu.Unlock()
	for _, sp := range dropped {
		s.event(cid, Row{"type": "task_split_resolved", "split_id": sp.ID, "action": "expired"})
	}
}
