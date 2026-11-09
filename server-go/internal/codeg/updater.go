// Updater: the codeg-ui update surface (src/lib/updater.ts). Version checks
// read an optional JSON manifest from HARNESS_HUB_UPDATE_MANIFEST; without one
// the server reports itself current. Applying updates needs a distribution
// channel this repository does not ship, so perform/rollback/restart say so
// explicitly instead of pretending.
package codeg

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"strings"
	"time"
)

const updaterVersion = "0.42.0-go"

func (s *Server) updaterCommand(command string, a Row) (any, error) {
	switch command {
	case "check_app_update":
		return s.updaterCheck()
	case "app_update_status":
		return Row{"currentVersion": updaterVersion, "selfUpdateSupported": false,
			"capability": "unsupported", "runtime": "standalone", "rollbackAvailable": false, "liveProgress": false}, nil
	case "app_update_state":
		return Row{"seq": 0, "status": "idle"}, nil
	case "perform_app_update":
		return nil, fmt.Errorf("本构建不内置更新通道，请从发布页手动更新")
	case "rollback_app":
		return nil, fmt.Errorf("本构建不内置回滚：保留旧版本目录即可手动回退")
	case "restart_app":
		return nil, fmt.Errorf("请关闭窗口后手动重启（后台服务继续运行）")
	}
	return nil, fmt.Errorf("未知的更新命令：%s", command)
}

// updaterCheck compares the running version against the optional manifest.
func (s *Server) updaterCheck() (any, error) {
	manifestURL := strings.TrimSpace(os.Getenv("HARNESS_HUB_UPDATE_MANIFEST"))
	if manifestURL == "" {
		return Row{"currentVersion": updaterVersion, "update": nil, "selfUpdateSupported": false,
			"capability": "unsupported", "runtime": "standalone", "restartDelayMs": 0,
			"rollbackAvailable": false, "liveProgress": false}, nil
	}
	client := &http.Client{Timeout: 15 * time.Second}
	response, err := client.Get(manifestURL)
	if err != nil {
		return nil, fmt.Errorf("无法读取更新清单：%v", err)
	}
	defer response.Body.Close()
	if response.StatusCode != 200 {
		return nil, fmt.Errorf("更新清单返回 %d", response.StatusCode)
	}
	var manifest struct {
		Version string `json:"version"`
		Notes   string `json:"body"`
		Date    string `json:"date"`
		URL     string `json:"url"`
	}
	if err := json.NewDecoder(response.Body).Decode(&manifest); err != nil {
		return nil, fmt.Errorf("更新清单格式无效：%v", err)
	}
	if manifest.Version == "" || manifest.Version == updaterVersion {
		return Row{"currentVersion": updaterVersion, "update": nil, "selfUpdateSupported": false,
			"capability": "unsupported", "runtime": "standalone", "restartDelayMs": 0,
			"rollbackAvailable": false, "liveProgress": false}, nil
	}
	return Row{"currentVersion": updaterVersion, "selfUpdateSupported": false,
		"update": Row{"version": manifest.Version, "body": manifest.Notes, "date": manifest.Date, "url": manifest.URL},
		"capability": "unsupported", "runtime": "standalone", "restartDelayMs": 0,
		"rollbackAvailable": false, "liveProgress": false}, nil
}
