package codeg

import "fmt"

func (s *Server) commitSettings(command string, a Row) (any, error) {
	st := s.store
	st.mu.Lock()
	defer st.mu.Unlock()
	key := "commit-message:" + fmt.Sprint(number(a["folderId"]))
	switch command {
	case "get_commit_message_settings":
		if saved, ok := st.Settings[key]; ok {
			return clone(saved), nil
		}
		if number(a["folderId"]) > 0 {
			return nil, nil
		}
		return Row{"agent": nil, "instructions": "", "auto_commit": false}, nil
	case "set_commit_message_settings":
		settings, ok := a["settings"].(map[string]any)
		if !ok {
			return nil, fmt.Errorf("提交设置必须是对象")
		}
		st.Settings[key] = settings
		return clone(settings), st.saveLocked()
	case "delete_commit_message_settings":
		delete(st.Settings, key)
		return nil, st.saveLocked()
	}
	return nil, fmt.Errorf("未知提交设置")
}
