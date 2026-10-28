package main

import (
	"bufio"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"os"
)

type Request struct {
	Version        int           `json:"version"`
	RequestID      string        `json:"request_id"`
	Method         string        `json:"method"`
	Path           string        `json:"path"`
	Query          [][2]string   `json:"query"`
	PathParameters [][2]string   `json:"path_parameters"`
	Headers        [][2]string   `json:"headers"`
	Body           []byte        `json:"body"`
}

type Response struct {
	Status  int          `json:"status"`
	Headers [][2]string  `json:"headers"`
	Body    []byte       `json:"body"`
}

type Error struct {
	Kind    string `json:"kind"`
	Message string `json:"message"`
}

type Frame struct {
	Version   int         `json:"version"`
	RequestID string      `json:"request_id"`
	Outcome   string      `json:"outcome"`
	Status    int         `json:"status"`
	Headers   [][2]string `json:"headers"`
	Body      string      `json:"body"`
	Error     *Error      `json:"error,omitempty"`
}

func handler(request Request) (Response, error) {
	name := "world"
	var payload map[string]interface{}
	if len(request.Body) > 0 {
		if err := json.Unmarshal(request.Body, &payload); err != nil {
			return Response{}, err
		}
		if value, ok := payload["name"].(string); ok {
			name = value
		}
	}
	greeting := os.Getenv("GREETING")
	if greeting == "" {
		greeting = "Hello"
	}
	body, err := json.Marshal(map[string]string{"message": fmt.Sprintf("%s, %s!", greeting, name)})
	if err != nil {
		return Response{}, err
	}
	return Response{
		Status:  200,
		Headers: [][2]string{{"content-type", "application/json"}},
		Body:    body,
	}, nil
}

func main() {
	scanner := bufio.NewScanner(os.Stdin)
	scanner.Buffer(make([]byte, 4096), 64*1024*1024)
	writer := bufio.NewWriter(os.Stdout)
	defer writer.Flush()
	for scanner.Scan() {
		var request Request
		if err := json.Unmarshal(scanner.Bytes(), &request); err != nil {
			fmt.Fprintln(os.Stderr, "invalid worker request:", err)
			continue
		}
		frame := Frame{Version: 1, RequestID: request.RequestID, Headers: [][2]string{}, Body: ""}
		if request.Version != 1 {
			frame.Outcome = "error"
			frame.Error = &Error{Kind: "user", Message: "unsupported protocol version"}
		} else if result, err := handler(request); err != nil {
			frame.Outcome = "error"
			frame.Error = &Error{Kind: "user", Message: err.Error()}
		} else {
			frame.Outcome = "response"
			frame.Status = result.Status
			if result.Headers != nil {
				frame.Headers = result.Headers
			}
			frame.Body = base64.StdEncoding.EncodeToString(result.Body)
		}
		if err := json.NewEncoder(writer).Encode(frame); err != nil {
			fmt.Fprintln(os.Stderr, "failed to encode worker response:", err)
			return
		}
		if err := writer.Flush(); err != nil {
			fmt.Fprintln(os.Stderr, "failed to flush worker response:", err)
			return
		}
	}
	if err := scanner.Err(); err != nil {
		fmt.Fprintln(os.Stderr, "failed to read worker request:", err)
		os.Exit(1)
	}
}
