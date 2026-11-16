package mail

import (
	"bufio"
	"context"
	"net"
	"strings"
	"sync"
	"testing"
	"time"
)

// fakeSMTP is a minimal plain-text SMTP server that records one message.
type fakeSMTP struct {
	ln       net.Listener
	mu       sync.Mutex
	from, to string
	data     string
	authSeen bool
}

func startFake(t *testing.T) *fakeSMTP {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	f := &fakeSMTP{ln: ln}
	t.Cleanup(func() { ln.Close() })
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go f.serve(conn)
		}
	}()
	return f
}

func (f *fakeSMTP) serve(conn net.Conn) {
	defer conn.Close()
	r := bufio.NewReader(conn)
	say := func(s string) { conn.Write([]byte(s + "\r\n")) } //nolint:errcheck
	say("220 fake ESMTP")
	for {
		line, err := r.ReadString('\n')
		if err != nil {
			return
		}
		cmd := strings.ToUpper(strings.TrimSpace(line))
		switch {
		case strings.HasPrefix(cmd, "EHLO"), strings.HasPrefix(cmd, "HELO"):
			// No STARTTLS and no AUTH on offer.
			say("250-fake")
			say("250 8BITMIME")
		case strings.HasPrefix(cmd, "AUTH"):
			f.mu.Lock()
			f.authSeen = true
			f.mu.Unlock()
			say("535 no")
		case strings.HasPrefix(cmd, "MAIL FROM:"):
			f.mu.Lock()
			f.from = strings.TrimSpace(line[10:])
			f.mu.Unlock()
			say("250 ok")
		case strings.HasPrefix(cmd, "RCPT TO:"):
			f.mu.Lock()
			f.to = strings.TrimSpace(line[8:])
			f.mu.Unlock()
			say("250 ok")
		case cmd == "DATA":
			say("354 go ahead")
			var b strings.Builder
			for {
				l, err := r.ReadString('\n')
				if err != nil {
					return
				}
				if l == ".\r\n" {
					break
				}
				b.WriteString(l)
			}
			f.mu.Lock()
			f.data = b.String()
			f.mu.Unlock()
			say("250 queued")
		case cmd == "QUIT":
			say("221 bye")
			return
		default:
			say("250 ok")
		}
	}
}

func TestSendPlain(t *testing.T) {
	f := startFake(t)
	s := &SMTP{Addr: f.ln.Addr().String(), From: "USIE <usie@example.test>", Timeout: 5 * time.Second}
	if err := s.Send(context.Background(), "ana@example.test", "Hello Ünïcode", "line one\n.leading dot\nend"); err != nil {
		t.Fatal(err)
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	if !strings.HasPrefix(f.from, "<usie@example.test>") || f.to != "<ana@example.test>" {
		t.Fatalf("envelope: %q %q", f.from, f.to)
	}
	for _, want := range []string{"To: ana@example.test\r\n", "Subject: =?utf-8?q?", "\r\n\r\nline one\r\n", "\r\n..leading dot\r\n"} {
		if !strings.Contains(f.data, want) {
			t.Errorf("message lacks %q:\n%s", want, f.data)
		}
	}
}

func TestRefusesCredentialsWithoutTLS(t *testing.T) {
	f := startFake(t)
	s := &SMTP{Addr: f.ln.Addr().String(), From: "usie@example.test", Username: "u", Password: "secret", Timeout: 5 * time.Second}
	err := s.Send(context.Background(), "ana@example.test", "x", "y")
	if err == nil || !strings.Contains(err.Error(), "STARTTLS") {
		t.Fatalf("expected a STARTTLS refusal, got %v", err)
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.authSeen {
		t.Fatal("credentials were offered over a clear-text connection")
	}
}

func TestRejectsHeaderInjection(t *testing.T) {
	s := &SMTP{Addr: "127.0.0.1:1", From: "usie@example.test"}
	for _, c := range [][2]string{{"a@example.test\r\nBcc: x@example.test", "s"}, {"a@example.test", "s\nBcc: x@example.test"}} {
		if err := s.Send(context.Background(), c[0], c[1], "b"); err != errHeader {
			t.Errorf("%q / %q: %v", c[0], c[1], err)
		}
	}
}
