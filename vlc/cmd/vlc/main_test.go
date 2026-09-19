package main

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

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

func TestHealthAndCORS(t *testing.T) {
	res := httptest.NewRecorder()
	server{}.routes().ServeHTTP(res, httptest.NewRequest(http.MethodGet, "/healthz", nil))
	if res.Code != http.StatusOK || res.Header().Get("Access-Control-Allow-Origin") != "*" {
		t.Fatalf("health response: %d, headers=%v", res.Code, res.Header())
	}
}
