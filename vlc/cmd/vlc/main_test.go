package main

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"testing"
)

func TestLimitedBufferDrainsAndCaps(t *testing.T) {
	var b limitedBuffer
	input := bytes.Repeat([]byte{'x'}, 300*1024)
	n, err := b.Write(input)
	if err != nil || n != len(input) {
		t.Fatalf("Write = %d, %v", n, err)
	}
	if b.Len() != 256*1024 {
		t.Fatalf("buffer length = %d", b.Len())
	}
}

func TestStripANSI(t *testing.T) {
	got := stripANSI("\x1b[31merror\x1b[0m: bad")
	if got != "error: bad" {
		t.Fatalf("stripANSI = %q", got)
	}
}

func TestCompileSuccess(t *testing.T) {
	s := server{compile: func(_ context.Context, source, filename string) ([]byte, string, error) {
		if source != "function main() {}" || filename != "playground.vl" {
			t.Fatalf("unexpected compiler input: %q %q", source, filename)
		}
		return []byte("nara"), "", nil
	}}.routes()
	req := httptest.NewRequest(http.MethodPost, "/v1/compile", strings.NewReader(`{"source":"function main() {}"}`))
	res := httptest.NewRecorder()
	s.ServeHTTP(res, req)
	if res.Code != http.StatusOK {
		t.Fatalf("status = %d, body = %s", res.Code, res.Body)
	}
	var body compileResponse
	if err := json.NewDecoder(res.Body).Decode(&body); err != nil {
		t.Fatal(err)
	}
	if !body.OK || body.BytecodeB64 == "" || body.Target != "naravm" {
		t.Fatalf("unexpected response: %+v", body)
	}
}

func TestCompileFailureReturnsDiagnostics(t *testing.T) {
	s := server{compile: func(context.Context, string, string) ([]byte, string, error) {
		return nil, "E201: undefined variable", &buildError{}
	}}.routes()
	req := httptest.NewRequest(http.MethodPost, "/v1/compile", strings.NewReader(`{"source":"bad"}`))
	res := httptest.NewRecorder()
	s.ServeHTTP(res, req)
	if res.Code != http.StatusUnprocessableEntity {
		t.Fatalf("status = %d", res.Code)
	}
	var body compileResponse
	if err := json.NewDecoder(res.Body).Decode(&body); err != nil {
		t.Fatal(err)
	}
	if body.OK || !strings.Contains(body.Diagnostics, "undefined variable") {
		t.Fatalf("unexpected response: %+v", body)
	}
}

func TestCompileMutableViewSuccess(t *testing.T) {
	src := "type Foo = object { value: u64, }; function bump(c: *Foo) { c.value = 1u64; } function main() { let c: *Foo = Foo { value = 1u64 }; bump(c); }"
	s := server{compile: func(_ context.Context, source, filename string) ([]byte, string, error) {
		if source != src {
			t.Fatalf("unexpected compiler input: %q", source)
		}
		return []byte("nara"), "", nil
	}}.routes()
	req := httptest.NewRequest(http.MethodPost, "/v1/compile", strings.NewReader(`{"source":`+strconv.Quote(src)+`}`))
	res := httptest.NewRecorder()
	s.ServeHTTP(res, req)
	if res.Code != http.StatusOK {
		t.Fatalf("status = %d, body = %s", res.Code, res.Body)
	}
}

func TestCompileReadonlyMutationReturns422(t *testing.T) {
	diag := "\x1b[31m[E310] Error:\x1b[0m cannot assign field `value` through read-only view `Counter`"
	s := server{compile: func(context.Context, string, string) ([]byte, string, error) {
		return nil, diag, &buildError{}
	}}.routes()
	req := httptest.NewRequest(http.MethodPost, "/v1/compile", strings.NewReader(`{"source":"type Counter = object { value: u64, }; function bad(v: Counter) { v.value = 1u64; }"}`))
	res := httptest.NewRecorder()
	s.ServeHTTP(res, req)
	if res.Code != http.StatusUnprocessableEntity {
		t.Fatalf("status = %d", res.Code)
	}
	var body compileResponse
	if err := json.NewDecoder(res.Body).Decode(&body); err != nil {
		t.Fatal(err)
	}
	// ANSI stripped, diagnostic preserved.
	if body.OK || !strings.Contains(body.Diagnostics, "read-only view") {
		t.Fatalf("unexpected response: %+v", body)
	}
	if strings.Contains(body.Diagnostics, "\x1b[") {
		t.Fatalf("ANSI not stripped: %q", body.Diagnostics)
	}
}

func TestHealthAndCORS(t *testing.T) {
	res := httptest.NewRecorder()
	server{}.routes().ServeHTTP(res, httptest.NewRequest(http.MethodGet, "/healthz", nil))
	if res.Code != http.StatusOK || res.Header().Get("Access-Control-Allow-Origin") != "*" {
		t.Fatalf("health response: %d, headers=%v", res.Code, res.Header())
	}
}
