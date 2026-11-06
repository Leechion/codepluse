package orch

import (
	"testing"
	"time"
)

func TestParseCron(t *testing.T) {
	cron, err := ParseCron("*/15 9-17 * * 1-5")
	if err != nil {
		t.Fatalf("合法 cron 被拒绝：%v", err)
	}
	if !cron[0][0] || !cron[0][15] || cron[0][1] {
		t.Fatalf("分钟步长解析错误")
	}
	if !cron[1][9] || !cron[1][17] || cron[1][18] {
		t.Fatalf("小时范围解析错误")
	}
	if !cron[4][1] || !cron[4][5] || cron[4][6] {
		t.Fatalf("周几范围解析错误")
	}
}

func TestParseCronSunday7(t *testing.T) {
	// 与 Node 版一致：字段范围 0-6，7 直接被拒绝（parseCron 里的 7→0 映射是防御性死代码）
	if _, err := ParseCron("0 12 * * 7"); err == nil {
		t.Fatalf("周字段 7 应被拒绝（范围 0-6）")
	}
}

func TestParseCronInvalid(t *testing.T) {
	for _, bad := range []string{"", "* * * *", "99 * * * *", "* 24 * * *", "*/0 * * * *", "a * * * *"} {
		if _, err := ParseCron(bad); err == nil {
			t.Fatalf("cron %q 应被拒绝", bad)
		}
	}
}

func TestCronMatchesAndNextRun(t *testing.T) {
	cron, _ := ParseCron("30 9 * * *")
	base := time.Date(2026, 3, 20, 8, 0, 0, 0, time.Local)
	if CronMatches(cron, base) {
		t.Fatalf("8:30 不应命中 9:30 的计划")
	}
	next, err := NextRun(cron, base)
	if err != nil {
		t.Fatalf("NextRun 失败：%v", err)
	}
	sameDay := next.Year() == base.Year() && next.Month() == base.Month() && next.Day() == base.Day()
	if next.Hour() != 9 || next.Minute() != 30 || !sameDay {
		t.Fatalf("下一次应是当天 9:30，得到 %v", next)
	}
	never, _ := ParseCron("0 0 30 2 *") // 2月30日不存在
	if _, err := NextRun(never, base); err == nil {
		t.Fatalf("一年内无匹配应报错")
	}
}

func TestExtractJSON(t *testing.T) {
	direct := `{"output":"ok","complete":true}`
	v, err := extractJSON(direct)
	if err != nil || v["output"] != "ok" {
		t.Fatalf("直接 JSON 解析失败：%v", err)
	}
	fenced := "说明如下\n```json\n{\"output\":\"fenced\"}\n```\n完成"
	v, err = extractJSON(fenced)
	if err != nil || v["output"] != "fenced" {
		t.Fatalf("围栏 JSON 解析失败：%v", err)
	}
	wrapped := "结果：{\"reply\":\"你好\"} 以上"
	v, err = extractJSON(wrapped)
	if err != nil || v["reply"] != "你好" {
		t.Fatalf("花括号切片解析失败：%v", err)
	}
	if _, err := extractJSON("完全不是 JSON"); err == nil {
		t.Fatalf("非 JSON 应报错")
	}
}

func TestSchemaKeyFor(t *testing.T) {
	cases := map[string]string{
		"宰相拆解任务": "plan", "宰相修正计划": "plan", "宰相汇总汇报": "report",
		"宰相对话": "decision", "宰相调整分工": "adjust", "会话移交简报": "briefing",
		"随便什么": "worker",
	}
	for stage, want := range cases {
		if got := schemaKeyFor(stage); got != want {
			t.Fatalf("schemaKeyFor(%q) = %q, want %q", stage, got, want)
		}
	}
}
