package authn

import (
	"bytes"
	"strings"
	"testing"
)

func TestPasswordRoundTrip(t *testing.T) {
	hash, err := HashPassword("correct horse battery")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(hash, "$argon2id$v=19$m=19456,t=2,p=1$") {
		t.Fatalf("unexpected format: %s", hash)
	}
	if ok, err := VerifyPassword(hash, "correct horse battery"); err != nil || !ok {
		t.Fatalf("verify: ok=%v err=%v", ok, err)
	}
	if ok, _ := VerifyPassword(hash, "wrong"); ok {
		t.Fatal("wrong password accepted")
	}
	other, _ := HashPassword("correct horse battery")
	if other == hash {
		t.Fatal("hashes must be salted")
	}
}

func TestVerifyRejectsMalformedHashes(t *testing.T) {
	for _, h := range []string{
		"", "plaintext", "$argon2i$v=19$m=1,t=1,p=1$c2FsdA$a2V5", "$argon2id$v=18$m=19456,t=2,p=1$c2FsdA$a2V5",
		"$argon2id$v=19$m=99999999,t=2,p=1$c2FsdA$a2V5", "$argon2id$v=19$m=19456,t=2,p=0$c2FsdA$a2V5",
		"$argon2id$v=19$m=19456,t=2,p=1$!!$a2V5", "$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$",
	} {
		if ok, err := VerifyPassword(h, "x"); ok || err == nil {
			t.Errorf("%q: ok=%v err=%v", h, ok, err)
		}
	}
}

func TestTokens(t *testing.T) {
	a, _ := NewToken("usie_")
	b, _ := NewToken("usie_")
	if a == b || !strings.HasPrefix(a, "usie_") || len(a) != 5+43 {
		t.Fatalf("bad tokens %q %q", a, b)
	}
	if bytes.Equal(HashToken(a), HashToken(b)) || len(HashToken(a)) != 32 {
		t.Fatal("bad token hashes")
	}
}
