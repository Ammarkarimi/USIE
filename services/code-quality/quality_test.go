package quality

import (
	"fmt"
	"strings"
	"testing"
)

func TestSecretsFindKnownFormatsWithoutStoringThem(t *testing.T) {
	key := "ghp_" + strings.Repeat("a1B2", 9)
	src := "token = \"" + key + "\"\n# AKIAIOSFODNN7EXAMPLE is the documented example key\n"
	raws := ScanSecrets("app/config.py", "source", []byte(src))
	if len(raws) != 1 || raws[0].Rule != RuleSecret || raws[0].StartLine != 1 {
		t.Fatalf("got %+v", raws)
	}
	for _, s := range []string{raws[0].Message, raws[0].Context} {
		if strings.Contains(s, key[4:]) {
			t.Fatalf("secret leaked into %q", s)
		}
	}
	if !strings.Contains(raws[0].Message, "GitHub token") {
		t.Fatalf("message %q", raws[0].Message)
	}
	// Private keys are reported in tests too. The header is split so this
	// file does not itself contain a key.
	header := "-----BEGIN RSA " + "PRIVATE KEY-----"
	body := "MIIEowIBAAKCAQEAu1SU1LfVLPHCozMxH2Mo4lgOEePzNm0tRgeLezV6ffAt0gun"
	raws = ScanSecrets("tests/fixture.pem", "test", []byte(header+"\n"+body+"\n"))
	if len(raws) != 1 {
		t.Fatalf("private key: %+v", raws)
	}
	// Code that only recognizes the header is not a key.
	if raws := ScanSecrets("pem.go", "source", []byte(`if strings.HasPrefix(s, "`+header+`") {`+"\n\treturn true\n}")); len(raws) != 0 {
		t.Fatalf("header in code: %+v", raws)
	}
}

func TestHardcodedPasswords(t *testing.T) {
	cases := []struct {
		path, category, src string
		want                bool
	}{
		{"db.py", "source", `DB_PASSWORD = "k8#Qz!v2Lm"`, true},
		{"cfg.json", "config", `{"password": "Tr0ub4dor&3"}`, true},
		{"main.go", "source", `apiKey := "a8f5f167f44f4964e6c998dee827110c"`, true},
		{".env", "config", `SMTP_PASSWORD=pQ7rT2vW9x`, true},
		{"db.py", "source", `password = os.environ["DB_PASSWORD"]`, false},
		{"db.py", "source", `password = "changeme123"`, false},
		{"db.py", "source", `password = "<your-password>"`, false},
		{"db.py", "source", `PASSWORD_ENV = "USIE_SMTP_PASSWORD"`, false},
		{"db.py", "source", `password_hash = "$2b$12$abcdefghijklmnop"`, false},
		{"db.py", "source", `password = "aaaaaaaa"`, false},
		{"test_db.py", "test", `password = "k8#Qz!v2Lm"`, false},
		{"README.md", "documentation", `password: "k8#Qz!v2Lm"`, false},
		{"app.tsx", "source", `<Input type="password" autoComplete="new-password" />`, false},
	}
	for _, c := range cases {
		got := len(ScanSecrets(c.path, c.category, []byte(c.src))) > 0
		if got != c.want {
			t.Errorf("%s %q: got %v, want %v", c.path, c.src, got, c.want)
		}
	}
}

func block(prefix string) string {
	var b strings.Builder
	for i := 0; i < 12; i++ {
		fmt.Fprintf(&b, "    total += compute(%s, %d)\n", prefix, i)
	}
	return b.String()
}

func TestDuplicationFindsCopiedBlocksAcrossFiles(t *testing.T) {
	d := NewDuplication(1000)
	shared := block("x")
	d.Add("a.py", []byte("def a():\n"+shared+"    return total\n"))
	d.Add("b.py", []byte("import os\n\n# copy\ndef b():\n\n"+shared+"    return total + 1\n"))
	d.Add("c.py", []byte("def c():\n"+block("y")))
	res := d.Result()
	if res.Files["a.py"].DuplicatedLines != 12 || res.Files["b.py"].DuplicatedLines != 12 || res.Files["c.py"].DuplicatedLines != 0 {
		t.Fatalf("duplicated lines: %+v", res.Files)
	}
	blocks := res.Files["b.py"].Blocks
	if len(blocks) != 1 || blocks[0].StartLine != 6 || blocks[0].EndLine != 17 || !strings.Contains(blocks[0].Message, "a.py:2") {
		t.Fatalf("blocks: %+v", blocks)
	}
	if res.Files["a.py"].Blocks[0].Context != blocks[0].Context {
		t.Fatal("the same code must have the same context in both places")
	}

	small := NewDuplication(5)
	small.Add("a.py", []byte(shared))
	if r := small.Result(); !r.Skipped || len(r.Files) != 0 {
		t.Fatalf("budget: %+v", r)
	}
}

func TestAssembleFingerprintsAreStableAndDistinct(t *testing.T) {
	rules := Index(Catalog([]Rule{{ID: "weak-hash", Category: "hotspot", Severity: "major"}, {ID: "function-length", Category: "smell", Severity: "minor"}}))
	raws := []Raw{
		{Rule: "weak-hash", StartLine: 3, Context: "c1"},
		{Rule: "weak-hash", StartLine: 9, Context: "c1"},
		{Rule: "function-length", StartLine: 1, Context: "f"},
		{Rule: "not-in-catalog", StartLine: 1, Context: "x"},
	}
	a := Assemble("x.py", false, raws, rules)
	if len(a) != 3 || a[0].Fingerprint == a[1].Fingerprint || a[0].Category != "hotspot" {
		t.Fatalf("got %+v", a)
	}
	moved := Assemble("x.py", false, []Raw{{Rule: "weak-hash", StartLine: 40, Context: "c1"}}, rules)
	if moved[0].Fingerprint != a[0].Fingerprint {
		t.Fatal("moving code changed the fingerprint")
	}
	if other := Assemble("y.py", false, raws[:1], rules); other[0].Fingerprint == a[0].Fingerprint {
		t.Fatal("different files share a fingerprint")
	}
	if tests := Assemble("x_test.py", true, raws, rules); len(tests) != 2 {
		t.Fatalf("smells must be dropped in tests: %+v", tests)
	}
}

func TestGate(t *testing.T) {
	if err := ValidateGate(DefaultGate()); err != nil {
		t.Fatal(err)
	}
	for _, bad := range [][]Condition{{{"nope", 1}}, {{"bugs", -1}}, {{"bugs", 1}, {"bugs", 2}}, {{"duplicated_lines_percent", 101}}} {
		if ValidateGate(bad) == nil {
			t.Errorf("accepted %+v", bad)
		}
	}
	m := map[string]float64{"new_bugs": 0, "new_vulnerabilities": 2, "blocker_findings": 0}
	res := Evaluate(DefaultGate(), m, true)
	if res.Status != "failed" || res.Conditions[1].Status != "failed" || *res.Conditions[1].Actual != 2 {
		t.Fatalf("got %+v", res)
	}
	first := Evaluate(DefaultGate(), m, false)
	if first.Status != "passed" || first.Conditions[1].Status != "no_baseline" || first.Conditions[1].Actual != nil {
		t.Fatalf("no baseline: %+v", first)
	}
	if SeverityGrade(map[string]int{"minor": 3, "critical": 1}) != "D" || SeverityGrade(nil) != "A" {
		t.Fatal("severity grade")
	}
	if ReviewGrade(0, 0) != "A" || ReviewGrade(1, 4) != "E" || ReviewGrade(4, 5) != "A" {
		t.Fatal("review grade")
	}
}
