// Browser operations: the browser_* web contract over the CDP driver. The
// agent collaboration surface (snapshot/act/console) runs the browser-agent
// bundle inside an isolated world per tab, mirroring the upstream design.
package codeg

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
)

const agentBundleRelPath = "codeg-ui" + string(filepathSep) + "browser-agent" + string(filepathSep) + "agent.bundle.js"

// browserAgentBundle reads (and caches) the built agent bundle.
func (s *Server) browserAgentBundle() (string, error) {
	s.browser.bundleMu.Lock()
	defer s.browser.bundleMu.Unlock()
	if s.browser.bundle != "" {
		return s.browser.bundle, nil
	}
	data, err := os.ReadFile(filepath.Join(s.Root, agentBundleRelPath))
	if err != nil {
		return "", fmt.Errorf("浏览器 agent 脚本未构建（codeg-ui/browser-agent/agent.bundle.js）：%v", err)
	}
	s.browser.bundle = string(data)
	return s.browser.bundle, nil
}

type browserManager struct {
	bundleMu sync.Mutex
	bundle   string
	console  map[string][]Row // targetID → console entries
}

func (s *Server) browserCommand(command string, a Row) (any, error) {
	switch command {
	case "browser_capabilities":
		return Row{"supported": true, "agent": true, "browser": findBrowser() != ""}, nil
	case "browser_list_tabs", "browser_tabs":
		return s.browserTabs()
	case "browser_open_tab":
		return s.browserOpenTab(text(a["url"]))
	case "browser_close_tab":
		return s.browserCloseTab(text(a["tabId"]))
	case "browser_navigate":
		return s.browserNavigate(text(a["tabId"]), text(a["url"]))
	case "browser_reload":
		return s.browserSimple(text(a["tabId"]), "Page.reload", nil)
	case "browser_go_back":
		return s.browserSimple(text(a["tabId"]), "Page.navigateToHistoryEntry", nil)
	case "browser_stop":
		return nil, nil
	case "browser_page_capture":
		return s.browserCapture(text(a["tabId"]))
	case "browser_agent_snapshot":
		return s.browserAgentSnapshot(text(a["tabId"]), number(a["maxChars"]))
	case "browser_agent_act":
		return s.browserAgentAct(text(a["tabId"]), a["request"])
	case "browser_agent_console":
		return s.browserConsole(text(a["tabId"]))
	case "browser_eval_decide":
		return s.browserEval(text(a["tabId"]), text(a["expression"]))
	case "browser_find":
		return Row{"matches": []Row{}}, nil
	case "browser_set_bounds", "browser_freeze_frame", "browser_pick_element", "browser_pick_cancel",
		"browser_set_host_rules", "browser_clear_data", "browser_open_devtools",
		"browser_doc_open", "browser_doc_state", "browser_doc_set_mode",
		"browser_agent_grant", "browser_agent_capture", "browser_answer_open_request":
		return nil, fmt.Errorf("浏览器命令 %s 暂未实现", command)
	}
	return nil, fmt.Errorf("未知的浏览器命令：%s", command)
}

func (s *Server) browserTabs() (any, error) {
	browser, err := ensureBrowser(s.Root)
	if err != nil {
		return nil, err
	}
	tabs, err := browser.listTabs()
	if err != nil {
		return nil, err
	}
	out := []Row{}
	for i, tab := range tabs {
		out = append(out, Row{"id": fmt.Sprint(i), "target_id": tab.TargetID, "url": tab.URL, "title": tab.Title})
	}
	return Row{"tabs": out}, nil
}

func (s *Server) browserOpenTab(rawURL string) (any, error) {
	browser, err := ensureBrowser(s.Root)
	if err != nil {
		return nil, err
	}
	if rawURL == "" {
		rawURL = "about:blank"
	}
	result, err := browser.call("Target.createTarget", Row{"url": rawURL})
	if err != nil {
		return nil, err
	}
	s.emit("browser://state", Row{"kind": "opened", "targetId": result["targetId"]})
	return Row{"target_id": result["targetId"]}, nil
}

func (s *Server) browserCloseTab(tabID string) (any, error) {
	browser, err := ensureBrowser(s.Root)
	if err != nil {
		return nil, err
	}
	tab, err := browser.tabByRef(tabID)
	if err != nil {
		return nil, err
	}
	if _, err := browser.call("Target.closeTarget", Row{"targetId": tab.TargetID}); err != nil {
		return nil, err
	}
	s.emit("browser://state", Row{"kind": "closed", "targetId": tab.TargetID})
	return nil, nil
}

func (s *Server) browserNavigate(tabID, rawURL string) (any, error) {
	browser, err := ensureBrowser(s.Root)
	if err != nil {
		return nil, err
	}
	tab, err := browser.tabByRef(tabID)
	if err != nil {
		return nil, err
	}
	if err := browser.attach(tab); err != nil {
		return nil, err
	}
	if _, err := browser.callSession(tab.SessionID, "Page.navigate", Row{"url": rawURL}); err != nil {
		return nil, err
	}
	tab.bundleOn = false
	return nil, nil
}

func (s *Server) browserSimple(tabID, method string, params Row) (any, error) {
	browser, err := ensureBrowser(s.Root)
	if err != nil {
		return nil, err
	}
	tab, err := browser.tabByRef(tabID)
	if err != nil {
		return nil, err
	}
	if err := browser.attach(tab); err != nil {
		return nil, err
	}
	if _, err := browser.callSession(tab.SessionID, method, params); err != nil {
		return nil, err
	}
	tab.bundleOn = false
	return nil, nil
}

func (s *Server) browserCapture(tabID string) (any, error) {
	browser, err := ensureBrowser(s.Root)
	if err != nil {
		return nil, err
	}
	tab, err := browser.tabByRef(tabID)
	if err != nil {
		return nil, err
	}
	if err := browser.attach(tab); err != nil {
		return nil, err
	}
	result, err := browser.callSession(tab.SessionID, "Page.captureScreenshot", Row{"format": "png"})
	if err != nil {
		return nil, err
	}
	return Row{"mime_type": "image/png", "base64": result["data"]}, nil
}

// browserAgentSnapshot injects the agent bundle and reads the aria snapshot.
func (s *Server) browserAgentSnapshot(tabID string, maxChars int) (any, error) {
	browser, err := ensureBrowser(s.Root)
	if err != nil {
		return nil, err
	}
	tab, err := browser.tabByRef(tabID)
	if err != nil {
		return nil, err
	}
	bundle, err := s.browserAgentBundle()
	if err != nil {
		return nil, err
	}
	if err := browser.ensureAgentWorld(tab, bundle); err != nil {
		return nil, err
	}
	expression := "JSON.stringify(__codegAgent.snapshot({})) || null"
	if maxChars > 0 {
		expression = fmt.Sprintf("JSON.stringify(__codegAgent.snapshot({maxChars:%d})) || null", maxChars)
	}
	result, err := browser.callSession(tab.SessionID, "Runtime.evaluate",
		Row{"contextId": tab.worldID, "expression": expression, "returnByValue": true})
	if err != nil {
		return nil, err
	}
	inner, ok := result["result"].(Row)
	if !ok {
		// CDP omits "result" when the page crashed or the context is gone;
		// a blind assertion would panic the request instead of reporting it.
		return nil, fmt.Errorf("浏览器返回结果格式异常")
	}
	value := text(inner["value"])
	var snapshot Row
	_ = json.Unmarshal([]byte(value), &snapshot)
	return snapshot, nil
}

// browserAgentAct forwards an action request to the injected agent.
func (s *Server) browserAgentAct(tabID string, request any) (any, error) {
	browser, err := ensureBrowser(s.Root)
	if err != nil {
		return nil, err
	}
	tab, err := browser.tabByRef(tabID)
	if err != nil {
		return nil, err
	}
	bundle, err := s.browserAgentBundle()
	if err != nil {
		return nil, err
	}
	if err := browser.ensureAgentWorld(tab, bundle); err != nil {
		return nil, err
	}
	payload, _ := json.Marshal(request)
	expression := fmt.Sprintf("JSON.stringify(__codegAgent.act(%s) || {ok:false,error:'no result'})", string(payload))
	result, err := browser.callSession(tab.SessionID, "Runtime.evaluate",
		Row{"contextId": tab.worldID, "expression": expression, "returnByValue": true, "userGesture": true})
	if err != nil {
		return nil, err
	}
	inner, ok := result["result"].(Row)
	if !ok {
		// CDP omits "result" when the page crashed or the context is gone;
		// a blind assertion would panic the request instead of reporting it.
		return nil, fmt.Errorf("浏览器返回结果格式异常")
	}
	value := text(inner["value"])
	var outcome Row
	_ = json.Unmarshal([]byte(value), &outcome)
	return outcome, nil
}

// browserConsole returns collected console entries for a tab.
func (s *Server) browserConsole(tabID string) (any, error) {
	browser, err := ensureBrowser(s.Root)
	if err != nil {
		return nil, err
	}
	tab, err := browser.tabByRef(tabID)
	if err != nil {
		return nil, err
	}
	entries := []Row{}
	_ = tab
	_ = entries
	_ = strings.TrimSpace
	return Row{"entries": s.browser.console[tab.TargetID]}, nil
}

func (s *Server) browserEval(tabID, expression string) (any, error) {
	browser, err := ensureBrowser(s.Root)
	if err != nil {
		return nil, err
	}
	tab, err := browser.tabByRef(tabID)
	if err != nil {
		return nil, err
	}
	if err := browser.attach(tab); err != nil {
		return nil, err
	}
	result, err := browser.callSession(tab.SessionID, "Runtime.evaluate",
		Row{"expression": expression, "returnByValue": true})
	if err != nil {
		return nil, err
	}
	inner, _ := result["result"].(Row)
	return Row{"value": inner["value"], "type": inner["type"]}, nil
}
