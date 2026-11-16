// Package mail sends plain-text notification emails over SMTP.
package mail

import (
	"context"
	"errors"
	"fmt"
	"mime"
	"net"
	"net/mail"
	"net/smtp"
	"strings"
	"time"
)

// Sender delivers one message.
type Sender interface {
	Send(ctx context.Context, to, subject, body string) error
}

// SMTP sends through a submission server. STARTTLS is used when the server
// offers it, and required whenever credentials are configured.
type SMTP struct {
	Addr     string // host:port
	Username string
	Password string
	From     string
	Timeout  time.Duration
}

var errHeader = errors.New("mail: header contains a line break")

func (s *SMTP) Send(ctx context.Context, to, subject, body string) error {
	if strings.ContainsAny(to+subject+s.From, "\r\n") {
		return errHeader
	}
	addr, err := mail.ParseAddress(to)
	if err != nil {
		return fmt.Errorf("mail: invalid recipient: %w", err)
	}
	from, err := mail.ParseAddress(s.From)
	if err != nil {
		return fmt.Errorf("mail: invalid sender: %w", err)
	}
	timeout := s.Timeout
	if timeout <= 0 {
		timeout = 20 * time.Second
	}
	host, _, err := net.SplitHostPort(s.Addr)
	if err != nil {
		return fmt.Errorf("mail: address: %w", err)
	}
	dialer := &net.Dialer{Timeout: timeout}
	conn, err := dialer.DialContext(ctx, "tcp", s.Addr)
	if err != nil {
		return fmt.Errorf("mail: connect: %w", err)
	}
	_ = conn.SetDeadline(time.Now().Add(timeout))
	c, err := smtp.NewClient(conn, host)
	if err != nil {
		_ = conn.Close()
		return fmt.Errorf("mail: %w", err)
	}
	defer func() { _ = c.Close() }()
	if ok, _ := c.Extension("STARTTLS"); ok {
		if err := c.StartTLS(tlsConfig(host)); err != nil {
			return fmt.Errorf("mail: starttls: %w", err)
		}
	} else if s.Username != "" {
		return errors.New("mail: the server does not offer STARTTLS; refusing to send credentials in clear text")
	}
	if s.Username != "" {
		if err := c.Auth(smtp.PlainAuth("", s.Username, s.Password, host)); err != nil {
			return fmt.Errorf("mail: authentication failed: %w", err)
		}
	}
	if err := c.Mail(from.Address); err != nil {
		return fmt.Errorf("mail: %w", err)
	}
	if err := c.Rcpt(addr.Address); err != nil {
		return fmt.Errorf("mail: %w", err)
	}
	w, err := c.Data()
	if err != nil {
		return fmt.Errorf("mail: %w", err)
	}
	msg := Format(from.String(), addr.Address, subject, body, time.Now())
	if _, err := w.Write([]byte(msg)); err != nil {
		return fmt.Errorf("mail: %w", err)
	}
	if err := w.Close(); err != nil {
		return fmt.Errorf("mail: %w", err)
	}
	return c.Quit()
}

// Format renders an RFC 5322 plain-text message with CRLF line endings.
func Format(from, to, subject, body string, now time.Time) string {
	var b strings.Builder
	b.WriteString("From: " + from + "\r\n")
	b.WriteString("To: " + to + "\r\n")
	b.WriteString("Subject: " + mime.QEncoding.Encode("utf-8", subject) + "\r\n")
	b.WriteString("Date: " + now.UTC().Format(time.RFC1123Z) + "\r\n")
	b.WriteString("MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n")
	for _, line := range strings.Split(strings.ReplaceAll(body, "\r\n", "\n"), "\n") {
		// Dot-stuffing is done by the SMTP client.
		b.WriteString(line + "\r\n")
	}
	return b.String()
}
