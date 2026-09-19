package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
)

const (
	maxSourceBytes = 256 * 1024
	compileTimeout = 10 * time.Second
)

type compileRequest struct {
	Source   string `json:"source"`
	Filename string `json:"filename,omitempty"`
}

type compileResponse struct {
	OK          bool   `json:"ok"`
	Target      string `json:"target,omitempty"`
	BytecodeB64 string `json:"bytecode_base64,omitempty"`
	Diagnostics string `json:"diagnostics,omitempty"`
	Error       string `json:"error,omitempty"`
}

type compiler struct {
	binary string
}

func (c compiler) compile(ctx context.Context, source, filename string) ([]byte, string, error) {
	dir, err := os.MkdirTemp("", "vlc-")
	if err != nil {
		return nil, "", fmt.Errorf("create compile directory: %w", err)
	}
	defer os.RemoveAll(dir)

	input := filepath.Join(dir, safeFilename(filename))
	output := filepath.Join(dir, "module.nara")
	if err := os.WriteFile(input, []byte(source), 0o600); err != nil {
		return nil, "", fmt.Errorf("write source: %w", err)
	}

	cmd := exec.CommandContext(ctx, c.binary, "build", input, "--target", "naravm", "--out", output)
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return nil, "", fmt.Errorf("create compiler pipe: %w", err)
	}
	if err := cmd.Start(); err != nil {
		return nil, "", fmt.Errorf("start compiler: %w", err)
	}
	// Drain stderr concurrently so a chatty compiler cannot block on a full
	// pipe while the parent waits. Keep only a bounded diagnostic prefix.
	var diagnostics limitedBuffer
	readDone := make(chan error, 1)
	go func() {
		_, err := io.Copy(&diagnostics, stderr)
		readDone <- err
	}()
	waitErr := cmd.Wait()
	readErr := <-readDone
	if readErr != nil {
		return nil, diagnostics.String(), fmt.Errorf("read compiler output: %w", readErr)
	}
	if waitErr != nil {
		return nil, diagnostics.String(), &buildError{err: waitErr}
	}
	bytecode, err := os.ReadFile(output)
	if err != nil {
		return nil, diagnostics.String(), fmt.Errorf("read compiler output: %w", err)
	}
	return bytecode, diagnostics.String(), nil
}

type limitedBuffer struct{ bytes.Buffer }

func (b *limitedBuffer) Write(p []byte) (int, error) {
	if b.Len() < 256*1024 {
		keep := p
		if remaining := 256*1024 - b.Len(); len(keep) > remaining {
			keep = keep[:remaining]
		}
		_, _ = b.Buffer.Write(keep)
	}
	// Report all bytes consumed: the pipe must be drained even after the
	// response-size cap is reached.
	return len(p), nil
}

func (b *limitedBuffer) String() string {
	return stripANSI(b.Buffer.String())
}

func stripANSI(s string) string {
	var out strings.Builder
	for i := 0; i < len(s); {
		if s[i] == 0x1b && i+1 < len(s) && s[i+1] == '[' {
			i += 2
			for i < len(s) && (s[i] < '@' || s[i] > '~') {
				i++
			}
			if i < len(s) {
				i++
			}
			continue
		}
		out.WriteByte(s[i])
		i++
	}
	return out.String()
}

type buildError struct{ err error }

func (e *buildError) Error() string { return "source did not compile" }
func (e *buildError) Unwrap() error { return e.err }

type server struct {
	compile func(context.Context, string, string) ([]byte, string, error)
}

func (s server) routes() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /healthz", s.health)
	mux.HandleFunc("POST /v1/compile", s.compileHandler)
	return cors(mux)
}

func (s server) health(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
}

func (s server) compileHandler(w http.ResponseWriter, r *http.Request) {
	if r.Method == http.MethodOptions {
		w.WriteHeader(http.StatusNoContent)
		return
	}
	r.Body = http.MaxBytesReader(w, r.Body, maxSourceBytes+16*1024)
	var req compileRequest
	decoder := json.NewDecoder(r.Body)
	if err := decoder.Decode(&req); err != nil {
		writeJSON(w, http.StatusBadRequest, compileResponse{Error: "request must be JSON with a source string"})
		return
	}
	if strings.TrimSpace(req.Source) == "" {
		writeJSON(w, http.StatusBadRequest, compileResponse{Error: "source must not be empty"})
		return
	}
	if len([]byte(req.Source)) > maxSourceBytes {
		writeJSON(w, http.StatusRequestEntityTooLarge, compileResponse{Error: "source exceeds 256 KiB"})
		return
	}
	filename := req.Filename
	if filename == "" {
		filename = "playground.vl"
	}

	ctx, cancel := context.WithTimeout(r.Context(), compileTimeout)
	defer cancel()
	bytecode, diagnostics, err := s.compile(ctx, req.Source, filename)
	if err != nil {
		status := http.StatusInternalServerError
		if errors.Is(err, context.DeadlineExceeded) || errors.Is(ctx.Err(), context.DeadlineExceeded) {
			status = http.StatusGatewayTimeout
		} else if _, ok := err.(*buildError); ok {
			status = http.StatusUnprocessableEntity
		}
		writeJSON(w, status, compileResponse{
			OK:          false,
			Diagnostics: diagnostics,
			Error:       err.Error(),
		})
		return
	}
	writeJSON(w, http.StatusOK, compileResponse{
		OK:          true,
		Target:      "naravm",
		BytecodeB64: base64.StdEncoding.EncodeToString(bytecode),
		Diagnostics: diagnostics,
	})
}

func cors(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Access-Control-Allow-Origin", "*")
		w.Header().Set("Access-Control-Allow-Headers", "Content-Type")
		w.Header().Set("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
		if r.Method == http.MethodOptions {
			w.WriteHeader(http.StatusNoContent)
			return
		}
		next.ServeHTTP(w, r)
	})
}

func safeFilename(name string) string {
	name = filepath.Base(name)
	if name == "." || name == string(filepath.Separator) || name == "" {
		return "playground.vl"
	}
	if !strings.HasSuffix(name, ".vl") {
		name += ".vl"
	}
	return name
}

func writeJSON(w http.ResponseWriter, status int, value any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	if err := json.NewEncoder(w).Encode(value); err != nil {
		log.Printf("write response: %v", err)
	}
}

func main() {
	addr := getenv("VLC_ADDR", ":8080")
	binary := getenv("VL_COMPILER", "vl")
	handler := server{compile: compiler{binary: binary}.compile}.routes()
	log.Printf("vlc listening on %s, compiler=%s", addr, binary)
	if err := http.ListenAndServe(addr, handler); err != nil {
		log.Fatal(err)
	}
}

func getenv(name, fallback string) string {
	if value := os.Getenv(name); value != "" {
		return value
	}
	return fallback
}
