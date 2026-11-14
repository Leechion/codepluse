package codeg

// fetchURL retrieves a web page for an agent.
//
// The harnesses' own web tools verify the target domain with Anthropic's
// safety service BEFORE any request goes out (Claude Code's WebFetch asks
// claude.ai whether the domain may be fetched). Where claude.ai is not
// reachable — the usual case on a network that blocks it — the verification
// fails and the agent gets "Unable to verify if domain … is safe to fetch"
// instead of the page, with no way forward. This tool does the plain HTTP GET
// from the machine the app runs on, so the page still reaches the model.

import (
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"golang.org/x/text/encoding/simplifiedchinese"
	"golang.org/x/text/encoding/traditionalchinese"

	"harnesshub/internal/textutil"
)

const (
	fetchMaxBytes    = 4 << 20   // response body cap
	fetchMaxText     = 200 << 10 // text handed to the model
	fetchTimeout     = 25 * time.Second
	fetchMaxRedirect = 5
)

// browserish UA: public sites routinely answer a bare Go client with 403.
const fetchUserAgent = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) codepluse/2.0 Safari/537.36"

func (s *Server) fetchURL(a Row) (Row, error) {
	raw := strings.TrimSpace(text(a["url"]))
	if raw == "" {
		return nil, fmt.Errorf("url 不能为空")
	}
	parsed, err := url.Parse(raw)
	if err != nil || parsed.Host == "" {
		return nil, fmt.Errorf("URL 无效：%s", raw)
	}
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		return nil, fmt.Errorf("只支持 http/https URL：%s", raw)
	}
	limit := fetchMaxText
	if v, ok := a["max_chars"].(float64); ok && v >= 1 {
		// Compare in float space: int(v) overflows to MinInt64 for huge
		// values (1e300), and a negative limit makes the slice below panic.
		if v >= float64(fetchMaxText) {
			limit = fetchMaxText
		} else {
			limit = int(v)
		}
	}
	// s.fetchClient blocks private/loopback/link-local targets at dial time;
	// tests inject a permissive client through the same seam.
	client := s.fetchClient
	if client == nil {
		client = safeFetchClient()
	}
	req, err := http.NewRequest(http.MethodGet, parsed.String(), nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("User-Agent", fetchUserAgent)
	req.Header.Set("Accept", "text/html,application/xhtml+xml,application/json;q=0.9,text/plain;q=0.8,*/*;q=0.7")
	req.Header.Set("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
	res, err := client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("抓取失败：%w", err)
	}
	defer res.Body.Close()
	body, err := io.ReadAll(io.LimitReader(res.Body, fetchMaxBytes+1))
	if err != nil {
		return nil, fmt.Errorf("读取响应失败：%w", err)
	}
	if len(body) > fetchMaxBytes {
		return nil, fmt.Errorf("页面过大（超过 %d 字节）", fetchMaxBytes)
	}
	contentType := res.Header.Get("Content-Type")
	payload := decodeCharset(body, contentType)
	text := payload
	if strings.Contains(strings.ToLower(contentType), "html") || looksLikeHTML(payload) {
		text = htmlToText(payload)
	}
	truncated := false
	if utf8.RuneCountInString(text) > limit {
		// limit is a character budget (see "chars" below); byte slicing would
		// split a multi-byte rune and produce invalid UTF-8.
		text = textutil.TruncateRunes(text, limit)
		truncated = true
	}
	finalURL := parsed.String()
	if res.Request != nil && res.Request.URL != nil {
		finalURL = res.Request.URL.String()
	}
	row := Row{
		"url":          finalURL,
		"status":       res.StatusCode,
		"content_type": contentType,
		"chars":        len([]rune(text)),
	}
	if truncated {
		row["truncated"] = true
	}
	if res.StatusCode >= 400 {
		// The body is the useful part of an error page (a 403 from a CDN
		// explains itself); hand it over instead of a bare status code.
		row["text"] = text
		return row, nil
	}
	row["text"] = text
	return row, nil
}

// looksLikeHTML catches pages served as text/plain but written as HTML.
func looksLikeHTML(body string) bool {
	head := strings.ToLower(body)
	if len(head) > 2048 {
		head = head[:2048]
	}
	return strings.Contains(head, "<html") || strings.Contains(head, "<body") ||
		strings.Contains(head, "<div") || strings.Contains(head, "<p>")
}

// htmlToText keeps the readable text of an HTML document: scripts, styles and
// comments go, block-level tags become line breaks, and the remaining entities
// are decoded. It is deliberately small — the model reads the text, not the
// markup.
func htmlToText(body string) string {
	var out strings.Builder
	out.Grow(len(body) / 2)
	lower := strings.ToLower(body)
	skipUntil := ""
	for i := 0; i < len(body); {
		if skipUntil != "" {
			end := strings.Index(lower[i:], skipUntil)
			if end < 0 {
				// The closing tag never arrives — an inline <svg> icon without a
				// closing tag is common, and a truncated document is worse.
				// Stop skipping instead of dropping the rest of the page: the
				// content the caller asked for is further down.
				skipUntil = ""
				continue
			}
			i += end + len(skipUntil)
			skipUntil = ""
			continue
		}
		if body[i] == '<' {
			end := strings.IndexByte(body[i:], '>')
			if end < 0 {
				break
			}
			tag := strings.ToLower(body[i+1 : i+end])
			switch {
			case strings.HasPrefix(tag, "script"), strings.HasPrefix(tag, "style"),
				strings.HasPrefix(tag, "noscript"):
				skipUntil = "</" + strings.Fields(tag)[0] + ">"
			case strings.HasPrefix(tag, "!--"):
				// The scan above stopped at the FIRST '>', which for a
				// single-line comment IS its terminator — the comment text is
				// already skipped, and looking for a further "-->" would eat
				// everything up to the next comment on the page.
				if !strings.HasSuffix(tag, "--") {
					skipUntil = "-->"
				}
			case strings.HasPrefix(tag, "br"), strings.HasPrefix(tag, "p"),
				strings.HasPrefix(tag, "div"), strings.HasPrefix(tag, "li"),
				strings.HasPrefix(tag, "tr"), strings.HasPrefix(tag, "h1"),
				strings.HasPrefix(tag, "h2"), strings.HasPrefix(tag, "h3"),
				strings.HasPrefix(tag, "h4"), strings.HasPrefix(tag, "h5"),
				strings.HasPrefix(tag, "h6"), strings.HasPrefix(tag, "section"),
				strings.HasPrefix(tag, "article"), strings.HasPrefix(tag, "table"):
				out.WriteByte('\n')
			}
			i += end + 1
			continue
		}
		out.WriteByte(body[i])
		i++
	}
	return collapseSpaces(decodeEntities(out.String()))
}

var entityReplacements = []struct{ from, to string }{
	{"&nbsp;", " "}, {"&amp;", "&"}, {"&lt;", "<"}, {"&gt;", ">"},
	{"&quot;", "\""}, {"&apos;", "'"}, {"&#39;", "'"}, {"&mdash;", "—"},
	{"&ndash;", "–"}, {"&hellip;", "…"}, {"&ldquo;", "“"}, {"&rdquo;", "”"},
	{"&lsquo;", "‘"}, {"&rsquo;", "’"}, {"&middot;", "·"},
}

// decodeEntities handles the named entities that actually show up in fetched
// pages, plus numeric ones.
func decodeEntities(s string) string {
	for _, rep := range entityReplacements {
		s = strings.ReplaceAll(s, rep.from, rep.to)
	}
	for {
		start := strings.Index(s, "&#")
		if start < 0 {
			break
		}
		end := strings.IndexByte(s[start:], ';')
		if end < 0 {
			break
		}
		raw := s[start+2 : start+end]
		code := 0
		if strings.HasPrefix(raw, "x") || strings.HasPrefix(raw, "X") {
			if v, err := strconv.ParseInt(raw[1:], 16, 32); err == nil {
				code = int(v)
			}
		} else if v, err := strconv.Atoi(raw); err == nil {
			code = v
		}
		if code <= 0 {
			s = s[:start] + s[start+end+1:]
			continue
		}
		s = s[:start] + string(rune(code)) + s[start+end+1:]
	}
	return s
}

// decodeCharset turns a response body into UTF-8 text.
//
// Plenty of Chinese sites still serve GB2312/GBK (weather.com.cn does), and the
// bytes decoded as UTF-8 come out as mojibake — worse than useless to a model
// that has to read the page. The charset comes from the response header first,
// then from the document's own meta tag; when neither says anything but the
// body is not valid UTF-8, GB18030 is the pragmatic guess.
func decodeCharset(body []byte, contentType string) string {
	charset := charsetFromContentType(contentType)
	if charset == "" {
		charset = charsetFromMeta(body)
	}
	switch normalizeCharset(charset) {
	case "gbk", "gb2312", "gb18030":
		if out, err := simplifiedchinese.GB18030.NewDecoder().Bytes(body); err == nil {
			return string(out)
		}
	case "big5":
		if out, err := traditionalchinese.Big5.NewDecoder().Bytes(body); err == nil {
			return string(out)
		}
	}
	if !utf8.Valid(body) {
		// No usable declaration and the bytes are not UTF-8: decode as GB18030
		// rather than handing the model mojibake.
		if out, err := simplifiedchinese.GB18030.NewDecoder().Bytes(body); err == nil {
			return string(out)
		}
	}
	return string(body)
}

func charsetFromContentType(contentType string) string {
	for _, part := range strings.Split(contentType, ";") {
		part = strings.TrimSpace(part)
		if len(part) > 7 && strings.EqualFold(part[:7], "charset") {
			value := strings.TrimSpace(part[7:])
			return strings.Trim(value, "\"'")
		}
	}
	return ""
}

// charsetFromMeta reads the document's own declaration — many servers omit the
// header but keep <meta charset="gb2312"> in the head.
func charsetFromMeta(body []byte) string {
	head := body
	if len(head) > 4096 {
		head = head[:4096]
	}
	lower := strings.ToLower(string(head))
	index := strings.Index(lower, "charset")
	for index >= 0 {
		rest := lower[index+len("charset"):]
		rest = strings.TrimLeft(rest, " \t=\"'")
		end := strings.IndexAny(rest, "\"' >;\r\n\t")
		if end <= 0 {
			break
		}
		value := rest[:end]
		if value != "" {
			return value
		}
		next := strings.Index(lower[index+1:], "charset")
		if next < 0 {
			break
		}
		index = index + 1 + next
	}
	return ""
}

func normalizeCharset(charset string) string {
	charset = strings.ToLower(strings.TrimSpace(charset))
	switch {
	case charset == "gb2312" || charset == "gbk" || charset == "gb18030":
		return "gbk"
	case charset == "big5" || charset == "big5-hkscs":
		return "big5"
	case charset == "utf-8" || charset == "utf8":
		return "utf-8"
	}
	return charset
}

// collapseSpaces trims trailing spaces per line and drops the runs of blank
// lines a tag-stripped document leaves behind.
func collapseSpaces(s string) string {
	lines := strings.Split(s, "\n")
	kept := make([]string, 0, len(lines))
	blank := 0
	for _, line := range lines {
		line = strings.Join(strings.Fields(line), " ")
		if line == "" {
			blank++
			if blank > 1 {
				continue
			}
			kept = append(kept, "")
			continue
		}
		blank = 0
		kept = append(kept, line)
	}
	return strings.TrimSpace(strings.Join(kept, "\n"))
}
