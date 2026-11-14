// Package api is the HTTP API gateway: authentication, authorization,
// rate limiting, audit logging and the REST surface documented in
// openapi.yaml. Every route is declared in the routes table; a test fails if
// a route is missing from the OpenAPI document.
package api

import (
	"context"
	"crypto/rand"
	"crypto/subtle"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"runtime/debug"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/collectors"
	"github.com/prometheus/client_golang/prometheus/promhttp"

	"github.com/ammarkarimi/usie/internal/platform/authn"
	"github.com/ammarkarimi/usie/internal/platform/blob"
	"github.com/ammarkarimi/usie/internal/platform/config"
	"github.com/ammarkarimi/usie/internal/store"
	orchestrator "github.com/ammarkarimi/usie/services/execution-orchestrator"
	graph "github.com/ammarkarimi/usie/services/knowledge-graph"
	analysis "github.com/ammarkarimi/usie/services/semantic-analysis"
)

const (
	sessionCookie = "usie_session"
	csrfHeader    = "X-CSRF-Token"
	tokenPrefix   = "usie_"
	maxJSONBody   = 1 << 20
)

type Server struct {
	Cfg     config.Config
	Store   *store.Store
	Queue   *orchestrator.Queue
	Graph   graph.Store
	Blobs   blob.Store
	Log     *slog.Logger
	Version string
	// Engine is nil when the engine binary is not available to the API
	// process; analysis then depends entirely on workers.
	Engine *analysis.Info
	// WebDir, when set, is served as the single-page application.
	WebDir string

	registry *prometheus.Registry
	requests *prometheus.CounterVec
	latency  *prometheus.HistogramVec
	limiter  *rateLimiter
	authLim  *rateLimiter
}

type handlerFunc func(s *Server, w http.ResponseWriter, r *http.Request, p *store.Principal) error

type route struct {
	Method string
	Path   string
	// MinRole is the weakest role allowed; "" marks a public route.
	MinRole string
	Handler handlerFunc
	// AuthLimited applies the strict credential-guessing rate limit.
	AuthLimited bool
	// Raw routes manage their own body (uploads) or stream (SSE).
	Raw bool
}

// apiError is an error with an HTTP status and a stable machine-readable code.
type apiError struct {
	Status  int
	Code    string
	Message string
}

func (e *apiError) Error() string { return e.Message }

func errBadRequest(format string, a ...any) error {
	return &apiError{http.StatusBadRequest, "invalid_request", fmt.Sprintf(format, a...)}
}

var (
	errNotFound     = &apiError{http.StatusNotFound, "not_found", "resource not found"}
	errUnauthorized = &apiError{http.StatusUnauthorized, "unauthenticated", "authentication required"}
	errForbidden    = &apiError{http.StatusForbidden, "forbidden", "your role does not permit this action"}
)

func (s *Server) Handler() http.Handler {
	s.registry = prometheus.NewRegistry()
	s.requests = prometheus.NewCounterVec(prometheus.CounterOpts{Name: "usie_http_requests_total", Help: "HTTP requests by route and status."}, []string{"method", "route", "status"})
	s.latency = prometheus.NewHistogramVec(prometheus.HistogramOpts{Name: "usie_http_request_duration_seconds", Help: "HTTP request latency by route.", Buckets: prometheus.DefBuckets}, []string{"route"})
	s.registry.MustRegister(s.requests, s.latency, collectors.NewGoCollector(), collectors.NewProcessCollector(collectors.ProcessCollectorOpts{}))
	s.registerQueueMetrics()
	s.limiter = newRateLimiter(s.Cfg.RateLimitPerMinute)
	s.authLim = newRateLimiter(s.Cfg.AuthRateLimitPerMinute)

	mux := http.NewServeMux()
	for _, rt := range routes() {
		mux.Handle(rt.Method+" "+rt.Path, s.wrap(rt))
	}
	mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
	})
	mux.HandleFunc("GET /readyz", func(w http.ResponseWriter, r *http.Request) {
		ctx, cancel := context.WithTimeout(r.Context(), 2*time.Second)
		defer cancel()
		if err := s.Store.Pool.Ping(ctx); err != nil {
			writeJSON(w, http.StatusServiceUnavailable, map[string]string{"status": "unavailable", "reason": "database"})
			return
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "ready"})
	})
	mux.Handle("GET /metrics", promhttp.HandlerFor(s.registry, promhttp.HandlerOpts{}))
	mux.HandleFunc("/api/", func(w http.ResponseWriter, r *http.Request) {
		s.writeError(w, r, &apiError{http.StatusNotFound, "not_found", "no such endpoint"})
	})
	if s.WebDir != "" {
		mux.Handle("/", spaHandler(s.WebDir))
	}
	return s.base(mux)
}

func (s *Server) registerQueueMetrics() {
	var mu sync.Mutex
	var at time.Time
	var queued, running int64
	read := func() (int64, int64) {
		mu.Lock()
		defer mu.Unlock()
		if time.Since(at) > 5*time.Second {
			ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
			defer cancel()
			if q, r, err := s.Queue.QueueDepth(ctx); err == nil {
				queued, running, at = q, r, time.Now()
			}
		}
		return queued, running
	}
	s.registry.MustRegister(
		prometheus.NewGaugeFunc(prometheus.GaugeOpts{Name: "usie_jobs_queued", Help: "Analysis jobs waiting for a worker."}, func() float64 { q, _ := read(); return float64(q) }),
		prometheus.NewGaugeFunc(prometheus.GaugeOpts{Name: "usie_jobs_running", Help: "Analysis jobs currently running."}, func() float64 { _, r := read(); return float64(r) }),
	)
}

type ctxKey int

const requestIDKey ctxKey = 0

func requestID(r *http.Request) string {
	id, _ := r.Context().Value(requestIDKey).(string)
	return id
}

// base applies to every request: request id, security headers, panic recovery.
func (s *Server) base(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		buf := make([]byte, 8)
		rand.Read(buf) //nolint:errcheck
		id := hex.EncodeToString(buf)
		r = r.WithContext(context.WithValue(r.Context(), requestIDKey, id))
		h := w.Header()
		h.Set("X-Request-Id", id)
		h.Set("X-Content-Type-Options", "nosniff")
		h.Set("X-Frame-Options", "DENY")
		h.Set("Referrer-Policy", "no-referrer")
		h.Set("Cross-Origin-Opener-Policy", "same-origin")
		// The SPA loads only same-origin scripts; Monaco needs blob: workers
		// and inline styles.
		h.Set("Content-Security-Policy", "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; worker-src 'self' blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")
		if s.Cfg.CookieSecure {
			h.Set("Strict-Transport-Security", "max-age=31536000; includeSubDomains")
		}
		defer func() {
			if rec := recover(); rec != nil {
				s.Log.Error("panic in handler", "request_id", id, "panic", fmt.Sprint(rec), "stack", string(debug.Stack()))
				s.writeError(w, r, errors.New("panic"))
			}
		}()
		next.ServeHTTP(w, r)
	})
}

type statusWriter struct {
	http.ResponseWriter
	status int
}

func (w *statusWriter) WriteHeader(code int) {
	if w.status == 0 {
		w.status = code
	}
	w.ResponseWriter.WriteHeader(code)
}

func (w *statusWriter) Write(b []byte) (int, error) {
	if w.status == 0 {
		w.status = http.StatusOK
	}
	return w.ResponseWriter.Write(b)
}

func (w *statusWriter) Flush() {
	if f, ok := w.ResponseWriter.(http.Flusher); ok {
		f.Flush()
	}
}

// wrap adds rate limiting, authentication, CSRF protection, authorization,
// metrics and access logging to a route.
func (s *Server) wrap(rt route) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		start := time.Now()
		sw := &statusWriter{ResponseWriter: w}
		var principal *store.Principal
		err := func() error {
			ip := clientIP(r)
			if !s.limiter.allow(ip) || (rt.AuthLimited && !s.authLim.allow(ip)) {
				sw.Header().Set("Retry-After", "30")
				return &apiError{http.StatusTooManyRequests, "rate_limited", "too many requests; retry later"}
			}
			if !rt.Raw {
				r.Body = http.MaxBytesReader(sw, r.Body, maxJSONBody)
			}
			p, err := s.authenticate(r)
			if err != nil {
				return err
			}
			principal = p
			if rt.MinRole != "" {
				if p == nil {
					return errUnauthorized
				}
				if !store.RoleAtLeast(p.Role, rt.MinRole) {
					s.audit(r, p, "authorization.denied", "route", rt.Method+" "+rt.Path, "denied", nil)
					return errForbidden
				}
			}
			// Cookie-authenticated state changes need the CSRF token. Bearer
			// tokens are not sent automatically by browsers and are exempt.
			if p != nil && p.Kind == "session" && r.Method != http.MethodGet && r.Method != http.MethodHead {
				if subtle.ConstantTimeCompare([]byte(r.Header.Get(csrfHeader)), []byte(p.CSRFToken)) != 1 {
					return &apiError{http.StatusForbidden, "csrf", "missing or invalid CSRF token"}
				}
			}
			return rt.Handler(s, sw, r, p)
		}()
		if err != nil {
			s.writeError(sw, r, err)
		}
		status := sw.status
		if status == 0 {
			status = http.StatusOK
		}
		s.requests.WithLabelValues(rt.Method, rt.Path, strconv.Itoa(status)).Inc()
		s.latency.WithLabelValues(rt.Path).Observe(time.Since(start).Seconds())
		attrs := []any{"request_id", requestID(r), "method", r.Method, "route", rt.Path, "status", status, "duration_ms", time.Since(start).Milliseconds(), "ip", clientIP(r)}
		if principal != nil {
			attrs = append(attrs, "user_id", principal.UserID, "organization_id", principal.OrgID)
		}
		s.Log.Info("request", attrs...)
	})
}

// authenticate resolves the caller from a bearer token or session cookie.
// It returns (nil, nil) for anonymous requests.
func (s *Server) authenticate(r *http.Request) (*store.Principal, error) {
	if header := r.Header.Get("Authorization"); header != "" {
		token, ok := strings.CutPrefix(header, "Bearer ")
		if !ok || !strings.HasPrefix(token, tokenPrefix) || len(token) > 200 {
			return nil, errUnauthorized
		}
		p, err := s.Store.PrincipalByToken(r.Context(), authn.HashToken(token))
		if errors.Is(err, store.ErrNotFound) {
			return nil, errUnauthorized
		}
		if err != nil {
			return nil, err
		}
		return &p, nil
	}
	cookie, err := r.Cookie(sessionCookie)
	if err != nil || cookie.Value == "" || len(cookie.Value) > 200 {
		return nil, nil
	}
	p, err := s.Store.PrincipalBySession(r.Context(), authn.HashToken(cookie.Value))
	if errors.Is(err, store.ErrNotFound) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	return &p, nil
}

func (s *Server) writeError(w http.ResponseWriter, r *http.Request, err error) {
	var ae *apiError
	var tooLarge *http.MaxBytesError
	switch {
	case errors.As(err, &ae):
	case errors.As(err, &tooLarge):
		ae = &apiError{http.StatusRequestEntityTooLarge, "too_large", "request body is too large"}
	case errors.Is(err, store.ErrNotFound), errors.Is(err, graph.ErrNotFound), errors.Is(err, orchestrator.ErrNotFound), errors.Is(err, blob.ErrNotFound):
		ae = errNotFound
	case errors.Is(err, store.ErrConflict):
		ae = &apiError{http.StatusConflict, "conflict", "a resource with that name already exists"}
	case errors.Is(err, context.Canceled):
		ae = &apiError{499, "cancelled", "request cancelled"}
	default:
		// Internal details stay in the log; the client gets the request id.
		s.Log.Error("internal error", "request_id", requestID(r), "error", err.Error())
		ae = &apiError{http.StatusInternalServerError, "internal", "internal error"}
	}
	writeJSON(w, ae.Status, map[string]any{"error": map[string]string{"code": ae.Code, "message": ae.Message, "request_id": requestID(r)}})
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.Header().Set("Cache-Control", "no-store")
	w.WriteHeader(status)
	json.NewEncoder(w).Encode(v) //nolint:errcheck
}

// decode parses a JSON body strictly: unknown fields and trailing data are errors.
func decode(r *http.Request, v any) error {
	if ct := r.Header.Get("Content-Type"); !strings.HasPrefix(ct, "application/json") {
		return &apiError{http.StatusUnsupportedMediaType, "unsupported_media_type", "Content-Type must be application/json"}
	}
	dec := json.NewDecoder(r.Body)
	dec.DisallowUnknownFields()
	if err := dec.Decode(v); err != nil {
		var tooLarge *http.MaxBytesError
		if errors.As(err, &tooLarge) {
			return err
		}
		return errBadRequest("invalid JSON body: %s", err)
	}
	if dec.More() {
		return errBadRequest("invalid JSON body: unexpected trailing data")
	}
	return nil
}

func (s *Server) audit(r *http.Request, p *store.Principal, action, targetType, targetID, outcome string, detail map[string]any) {
	e := store.AuditEvent{Action: action, TargetType: targetType, TargetID: targetID, Outcome: outcome, IP: clientIP(r), RequestID: requestID(r), Detail: detail}
	if p != nil {
		e.OrgID, e.ActorID, e.ActorKind = p.OrgID, &p.UserID, p.Kind
	}
	ctx, cancel := context.WithTimeout(context.WithoutCancel(r.Context()), 5*time.Second)
	defer cancel()
	if err := s.Store.Audit(ctx, e); err != nil {
		// An audit failure is surfaced loudly but does not undo the action.
		s.Log.Error("audit write failed", "request_id", requestID(r), "action", action, "error", err.Error())
	}
}

// clientIP is the peer address. Forwarding headers are not trusted: behind a
// reverse proxy, rate limits apply per proxy unless the proxy enforces its own.
func clientIP(r *http.Request) string {
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	if err != nil {
		return r.RemoteAddr
	}
	return host
}

func queryInt(r *http.Request, name string, def, min, max int) (int, error) {
	raw := r.URL.Query().Get(name)
	if raw == "" {
		return def, nil
	}
	n, err := strconv.Atoi(raw)
	if err != nil || n < min || n > max {
		return 0, errBadRequest("%s must be an integer between %d and %d", name, min, max)
	}
	return n, nil
}

func queryList(r *http.Request, name string) []string {
	var out []string
	for _, part := range strings.Split(r.URL.Query().Get(name), ",") {
		if part = strings.TrimSpace(part); part != "" {
			out = append(out, part)
		}
	}
	return out
}

// pathID returns a UUID path parameter or a 404 for anything else.
func pathID(r *http.Request, name string) (string, error) {
	id := r.PathValue(name)
	if !store.ValidID(id) {
		return "", errNotFound
	}
	return strings.ToLower(id), nil
}

// ---- rate limiting ----------------------------------------------------------

// rateLimiter is a per-key token bucket held in memory. With several API
// replicas the effective limit is per replica.
type rateLimiter struct {
	mu      sync.Mutex
	perMin  float64
	buckets map[string]*bucket
	swept   time.Time
}

type bucket struct {
	tokens float64
	seen   time.Time
}

func newRateLimiter(perMinute int) *rateLimiter {
	return &rateLimiter{perMin: float64(perMinute), buckets: map[string]*bucket{}, swept: time.Now()}
}

func (l *rateLimiter) allow(key string) bool {
	now := time.Now()
	l.mu.Lock()
	defer l.mu.Unlock()
	if now.Sub(l.swept) > time.Minute {
		for k, b := range l.buckets {
			if now.Sub(b.seen) > 2*time.Minute {
				delete(l.buckets, k)
			}
		}
		l.swept = now
	}
	b, ok := l.buckets[key]
	if !ok {
		b = &bucket{tokens: l.perMin, seen: now}
		l.buckets[key] = b
	}
	b.tokens += now.Sub(b.seen).Minutes() * l.perMin
	if b.tokens > l.perMin {
		b.tokens = l.perMin
	}
	b.seen = now
	if b.tokens < 1 {
		return false
	}
	b.tokens--
	return true
}

// spaHandler serves the built web application, falling back to index.html
// for client-side routes.
func spaHandler(dir string) http.Handler {
	files := http.FileServer(http.Dir(dir))
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet && r.Method != http.MethodHead {
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}
		if strings.HasPrefix(r.URL.Path, "/assets/") {
			w.Header().Set("Cache-Control", "public, max-age=31536000, immutable")
			files.ServeHTTP(w, r)
			return
		}
		if strings.Contains(r.URL.Path[strings.LastIndex(r.URL.Path, "/")+1:], ".") {
			files.ServeHTTP(w, r)
			return
		}
		w.Header().Set("Cache-Control", "no-cache")
		http.ServeFile(w, r, dir+"/index.html")
	})
}
