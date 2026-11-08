package codeg

import (
	"fmt"
	"math"
	"sort"
	"strings"
)

func findRow(rows []Row, id int) Row {
	for _, r := range rows {
		if number(r["id"]) == id {
			return r
		}
	}
	return nil
}
func float(v any) float64 {
	switch n := v.(type) {
	case float64:
		return n
	case int:
		return float64(n)
	}
	return 0
}
func canvasGeometry(key string, value any) (float64, error) {
	n := float(value)
	if math.IsNaN(n) || math.IsInf(n, 0) {
		return 0, fmt.Errorf("画布坐标必须是有限数值")
	}
	if key == "width" || key == "height" {
		return math.Max(48, math.Min(20000, n)), nil
	}
	return math.Max(-1000000, math.Min(1000000, n)), nil
}
func ids(value any) []int {
	out := []int{}
	switch list := value.(type) {
	case []any:
		for _, v := range list {
			out = append(out, number(v))
		}
	case []int:
		out = append(out, list...)
	}
	return out
}
func hasID(list []int, id int) bool {
	for _, n := range list {
		if n == id {
			return true
		}
	}
	return false
}
func nullable(v any) any {
	str := strings.TrimSpace(text(v))
	if str == "" {
		return nil
	}
	return str
}
func regionKind(kind any) bool {
	return kind == "custom" || kind == "folder" || kind == "group" || kind == "agent"
}
func (s *Store) makeCanvasNodeLocked(a Row) (Row, error) {
	board := number(a["boardId"])
	if findRow(s.CanvasBoards, board) == nil {
		return nil, fmt.Errorf("画布不存在")
	}
	kind := text(a["kind"])
	if !regionKind(kind) && kind != "conversation" && kind != "note" && kind != "file" && kind != "terminal" {
		return nil, fmt.Errorf("不支持的画布节点类型")
	}
	r := Row{"board_id": board, "kind": kind, "folder_id": nil, "folder_group_id": nil, "agent_type": nil, "conversation_id": nil, "path": nil, "member_ids": []int{}, "title": nullable(a["title"]), "content": nullable(a["content"]), "color": nullable(a["color"]), "collapsed": false, "grid_columns": 0, "grid_rows": 0, "created_at": timestamp(), "updated_at": timestamp()}
	binding := map[string]string{"folder": "folderId", "group": "folderGroupId", "agent": "agentType", "conversation": "conversationId", "file": "path", "terminal": "path"}[kind]
	if binding != "" {
		v := a[binding]
		if ((binding == "agentType" || binding == "path") && text(v) == "") || ((binding != "agentType" && binding != "path") && number(v) <= 0) {
			return nil, fmt.Errorf("%s 节点需要 %s", kind, binding)
		}
		wire := map[string]string{"folderId": "folder_id", "folderGroupId": "folder_group_id", "agentType": "agent_type", "conversationId": "conversation_id", "path": "path"}[binding]
		r[wire] = v
	}
	for key, wire := range map[string]string{"folderId": "folder_id", "folderGroupId": "folder_group_id", "agentType": "agent_type", "conversationId": "conversation_id", "path": "path"} {
		if key != binding && a[key] != nil {
			return nil, fmt.Errorf("%s 节点不能绑定 %s", kind, wire)
		}
	}
	for _, key := range []string{"x", "y", "width", "height"} {
		if a[key] == nil {
			return nil, fmt.Errorf("缺少节点几何字段 %s", key)
		}
		v, err := canvasGeometry(key, a[key])
		if err != nil {
			return nil, err
		}
		r[key] = v
	}
	if regionKind(kind) {
		r["grid_columns"] = max(0, min(12, number(a["gridColumns"])))
		r["grid_rows"] = max(0, min(12, number(a["gridRows"])))
	}
	r["id"] = s.allocate()
	return r, nil
}
func (s *Server) canvasCommand(command string, a Row) (out any, err error) {
	st := s.store
	st.mu.Lock()
	defer st.mu.Unlock()
	beforeBoards, beforeNodes, beforeNext, beforeRevision := clone(st.CanvasBoards), clone(st.CanvasNodes), st.Next, st.CanvasRevision
	defer func() {
		if err != nil {
			st.CanvasBoards, st.CanvasNodes, st.Next, st.CanvasRevision = beforeBoards, beforeNodes, beforeNext, beforeRevision
		}
	}()
	boardEvent := func(kind string, r Row) {
		if kind == "deleted" {
			s.emit("canvas-board://changed", Row{"kind": kind, "id": r["id"]})
		} else {
			s.emit("canvas-board://changed", Row{"kind": kind, "board": clone(r)})
		}
	}
	touch := func(boardID int) {
		if b := findRow(st.CanvasBoards, boardID); b != nil {
			b["updated_at"] = timestamp()
		}
	}
	commit := func(event Row, value any) (any, error) {
		st.CanvasRevision++
		event["revision"] = st.CanvasRevision
		if err := st.saveLocked(); err != nil {
			return nil, err
		}
		s.emit("canvas://changed", clone(event))
		return Row{"value": clone(value), "revision": st.CanvasRevision}, nil
	}
	prune := func(remove []int) []int {
		kept := []Row{}
		deleted := []int{}
		for _, n := range st.CanvasNodes {
			if hasID(remove, number(n["id"])) {
				deleted = append(deleted, number(n["id"]))
				touch(number(n["board_id"]))
			} else {
				kept = append(kept, n)
			}
		}
		st.CanvasNodes = kept
		return deleted
	}
	killDeleted := func(remove []int) {
		for _, id := range remove {
			_, _ = s.terminalCommand("terminal_kill", Row{"terminalId": fmt.Sprintf("canvas-term-%d", id)})
		}
	}
	input, _ := a["input"].(map[string]any)
	switch command {
	case "canvas_list_boards":
		result := []Row{}
		for _, b := range st.CanvasBoards {
			preview := []Row{}
			terminalCount := 0
			for _, n := range st.CanvasNodes {
				if number(n["board_id"]) != number(b["id"]) {
					continue
				}
				preview = append(preview, Row{"kind": n["kind"], "x": n["x"], "y": n["y"], "width": n["width"], "height": n["height"], "color": n["color"]})
				if n["kind"] == "terminal" {
					terminalCount++
				}
			}
			count := len(preview)
			sort.SliceStable(preview, func(i, j int) bool {
				return float(preview[i]["width"])*float(preview[i]["height"]) > float(preview[j]["width"])*float(preview[j]["height"])
			})
			if len(preview) > 32 {
				preview = preview[:32]
			}
			result = append(result, Row{"board": clone(b), "node_count": count, "terminal_count": terminalCount, "preview": preview})
		}
		sort.SliceStable(result, func(i, j int) bool {
			return text(result[i]["board"].(map[string]any)["updated_at"]) > text(result[j]["board"].(map[string]any)["updated_at"])
		})
		return result, nil
	case "canvas_create_board":
		b := Row{"id": st.allocate(), "name": nullable(input["name"]), "description": nullable(input["description"]), "color": nullable(input["color"]), "created_at": timestamp(), "updated_at": timestamp()}
		st.CanvasBoards = append(st.CanvasBoards, b)
		if err = st.saveLocked(); err != nil {
			return nil, err
		}
		boardEvent("upsert", b)
		return clone(b), nil
	case "canvas_update_board":
		b := findRow(st.CanvasBoards, number(a["boardId"]))
		if b == nil {
			return nil, fmt.Errorf("画布不存在")
		}
		patch, _ := a["patch"].(map[string]any)
		for _, key := range []string{"name", "description", "color"} {
			if v, ok := patch[key]; ok {
				b[key] = nullable(v)
			}
		}
		touch(number(b["id"]))
		if err = st.saveLocked(); err != nil {
			return nil, err
		}
		boardEvent("upsert", b)
		return clone(b), nil
	case "canvas_list_nodes":
		board := number(a["boardId"])
		if findRow(st.CanvasBoards, board) == nil {
			return nil, fmt.Errorf("画布不存在")
		}
		nodes := []Row{}
		for _, n := range st.CanvasNodes {
			if number(n["board_id"]) == board {
				nodes = append(nodes, clone(n))
			}
		}
		return Row{"board_id": board, "nodes": nodes, "revision": st.CanvasRevision}, nil
	case "canvas_create_node":
		if kind := text(input["kind"]); kind == "file" || kind == "terminal" {
			p, e := s.Resolve(text(input["path"]))
			if e != nil {
				return nil, e
			}
			input["path"] = p
		}
		n, e := st.makeCanvasNodeLocked(input)
		if e != nil {
			return nil, e
		}
		st.CanvasNodes = append(st.CanvasNodes, n)
		touch(number(n["board_id"]))
		return commit(Row{"kind": "upsert", "node": n}, n)
	case "canvas_update_node":
		n := findRow(st.CanvasNodes, number(a["nodeId"]))
		if n == nil {
			return nil, fmt.Errorf("画布节点不存在")
		}
		patch, _ := a["patch"].(map[string]any)
		for _, key := range []string{"title", "content", "color"} {
			if v, ok := patch[key]; ok {
				n[key] = nullable(v)
			}
		}
		for _, key := range []string{"x", "y", "width", "height"} {
			if v, ok := patch[key]; ok {
				n[key], err = canvasGeometry(key, v)
				if err != nil {
					return nil, err
				}
			}
		}
		if v, ok := patch["collapsed"]; ok {
			if _, valid := v.(bool); !valid {
				return nil, fmt.Errorf("collapsed 必须为布尔值")
			}
			n["collapsed"] = v
		}
		for key, wire := range map[string]string{"gridColumns": "grid_columns", "gridRows": "grid_rows"} {
			if v, ok := patch[key]; ok {
				if !regionKind(n["kind"]) {
					return nil, fmt.Errorf("只有区域支持网格设置")
				}
				n[wire] = max(0, min(12, number(v)))
			}
		}
		members := ids(n["member_ids"])
		for _, key := range []string{"memberAdd", "memberRemove"} {
			if v, ok := patch[key]; ok {
				if n["kind"] != "custom" {
					return nil, fmt.Errorf("只有自定义区域可修改成员")
				}
				id := number(v)
				if key == "memberAdd" {
					if st.conversationLocked(id) == nil {
						return nil, fmt.Errorf("成员会话不存在")
					}
					if !hasID(members, id) {
						members = append(members, id)
					}
				} else {
					filtered := []int{}
					for _, m := range members {
						if m != id {
							filtered = append(filtered, m)
						}
					}
					members = filtered
				}
			}
		}
		if len(members) > 200 {
			return nil, fmt.Errorf("区域最多包含 200 个会话")
		}
		n["member_ids"] = members
		n["updated_at"] = timestamp()
		touch(number(n["board_id"]))
		return commit(Row{"kind": "upsert", "node": n}, n)
	case "canvas_move_nodes":
		moves, _ := a["moves"].([]any)
		applied := []Row{}
		for _, raw := range moves {
			m, _ := raw.(map[string]any)
			n := findRow(st.CanvasNodes, number(m["id"]))
			if n == nil {
				continue
			}
			x, e := canvasGeometry("x", m["x"])
			if e != nil {
				return nil, e
			}
			y, e := canvasGeometry("y", m["y"])
			if e != nil {
				return nil, e
			}
			n["x"], n["y"], n["updated_at"] = x, y, timestamp()
			touch(number(n["board_id"]))
			applied = append(applied, Row{"id": n["id"], "x": x, "y": y})
		}
		if len(applied) == 0 {
			return Row{"value": applied, "revision": st.CanvasRevision}, nil
		}
		return commit(Row{"kind": "moved", "moves": applied}, applied)
	case "canvas_delete_node", "canvas_delete_nodes", "canvas_delete_board":
		remove := ids(a["nodeIds"])
		if command == "canvas_delete_node" {
			remove = []int{number(a["nodeId"])}
		}
		if command == "canvas_delete_board" {
			board := number(a["boardId"])
			for _, n := range st.CanvasNodes {
				if number(n["board_id"]) == board {
					remove = append(remove, number(n["id"]))
				}
			}
			kept := []Row{}
			for _, b := range st.CanvasBoards {
				if number(b["id"]) != board {
					kept = append(kept, b)
				}
			}
			st.CanvasBoards = kept
		}
		deleted := prune(remove)
		var result any = deleted
		event := Row{"kind": "pruned", "deleted_ids": deleted, "updated": []Row{}}
		if command == "canvas_delete_node" {
			event = Row{"kind": "deleted", "id": number(a["nodeId"])}
			result = nil
		}
		response, e := commit(event, result)
		if e != nil {
			return nil, e
		}
		killDeleted(deleted)
		if command == "canvas_delete_board" {
			boardEvent("deleted", Row{"id": number(a["boardId"])})
		}
		return response, nil
	case "canvas_group_into_region":
		board := number(input["boardId"])
		n := findRow(st.CanvasNodes, number(input["targetRegionId"]))
		if number(input["targetRegionId"]) > 0 && (n == nil || n["kind"] != "custom" || number(n["board_id"]) != board) {
			return nil, fmt.Errorf("目标必须是当前画布的自定义区域")
		}
		members := []int{}
		if n != nil {
			members = ids(n["member_ids"])
		}
		for _, id := range ids(input["memberIds"]) {
			if st.conversationLocked(id) == nil {
				return nil, fmt.Errorf("成员会话不存在")
			}
			if !hasID(members, id) {
				members = append(members, id)
			}
		}
		if len(members) > 200 {
			return nil, fmt.Errorf("区域最多包含 200 个会话")
		}
		if n == nil {
			copy := clone(input)
			copy["kind"] = "custom"
			delete(copy, "targetRegionId")
			n, err = st.makeCanvasNodeLocked(copy)
			if err != nil {
				return nil, err
			}
			st.CanvasNodes = append(st.CanvasNodes, n)
		}
		n["member_ids"] = members
		n["updated_at"] = timestamp()
		remove := []int{}
		for _, id := range ids(input["consumeNodeIds"]) {
			pin := findRow(st.CanvasNodes, id)
			if pin != nil && pin["kind"] == "conversation" && number(pin["board_id"]) == board && hasID(members, number(pin["conversation_id"])) {
				remove = append(remove, id)
			}
		}
		deleted := prune(remove)
		touch(board)
		return commit(Row{"kind": "grouped", "node": n, "deleted_ids": deleted}, Row{"node": n, "deletedIds": deleted})
	case "canvas_detach_member":
		region := findRow(st.CanvasNodes, number(a["regionId"]))
		conversation := number(a["conversationId"])
		if region == nil || !regionKind(region["kind"]) || st.conversationLocked(conversation) == nil {
			return nil, fmt.Errorf("区域或成员会话不存在")
		}
		var removed any
		if region["kind"] == "custom" {
			members := ids(region["member_ids"])
			if !hasID(members, conversation) {
				return nil, fmt.Errorf("成员已移出区域")
			}
			kept := []int{}
			for _, id := range members {
				if id != conversation {
					kept = append(kept, id)
				}
			}
			region["member_ids"] = kept
			removed = region["id"]
		}
		n, e := st.makeCanvasNodeLocked(Row{"boardId": region["board_id"], "kind": "conversation", "conversationId": conversation, "x": a["x"], "y": a["y"], "width": 320, "height": 220})
		if e != nil {
			return nil, e
		}
		st.CanvasNodes = append(st.CanvasNodes, n)
		touch(number(region["board_id"]))
		return commit(Row{"kind": "detached", "removed_from": removed, "node": n}, n)
	}
	return nil, fmt.Errorf("未知画布接口：%s", command)
}
