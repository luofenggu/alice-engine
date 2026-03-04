#!/usr/bin/env python3
"""
Mock LLM Server for manual testing.
Impersonates OpenAI-compatible API, returns scripted responses.
"""

import json
import re
import sys
from http.server import HTTPServer, BaseHTTPRequestHandler

# Script queue - each entry is consumed once per inference request
SCRIPTS = []
script_index = 0

def extract_action_token(body):
    """Extract action token from system prompt."""
    messages = body.get("messages", [])
    for msg in messages:
        content = msg.get("content", "")
        match = re.search(r'###ACTION_[a-f0-9]+###', content)
        if match:
            return match.group(0)
    return None

def extract_last_user_content(body):
    """Extract last user message content."""
    messages = body.get("messages", [])
    for msg in reversed(messages):
        if msg.get("role") == "user":
            return msg.get("content", "")
    return ""

def format_sse(content):
    """Format as SSE streaming response."""
    content_json = json.dumps(content)
    return f'data: {{"choices":[{{"delta":{{"content":{content_json}}}}}]}}\n\ndata: [DONE]\n\n'

class MockHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        global script_index
        
        content_length = int(self.headers.get('Content-Length', 0))
        body_str = self.rfile.read(content_length).decode('utf-8')
        body = json.loads(body_str)
        
        token = extract_action_token(body)
        user_content = extract_last_user_content(body)
        user_preview = user_content[:60]
        
        if not token:
            # Compress request - no action token
            print(f"[MOCK-LLM] Compress request, returning fixed response | user: {user_preview}")
            response = "Compressed: hello world test session."
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Cache-Control', 'no-cache')
            self.end_headers()
            self.wfile.write(format_sse(response).encode())
            return
        
        if script_index < len(SCRIPTS):
            script = SCRIPTS[script_index]
            script_index += 1
            content = script.replace("{ACTION_TOKEN}", token)
            print(f"[MOCK-LLM] Script #{script_index}/{len(SCRIPTS)} consumed | user: {user_preview}")
        else:
            # All scripts consumed - return idle
            content = f"{token}-idle\n"
            print(f"[MOCK-LLM] All scripts consumed, returning idle | user: {user_preview}")
        
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Cache-Control', 'no-cache')
        self.end_headers()
        self.wfile.write(format_sse(content).encode())
    
    def log_message(self, format, *args):
        pass  # Suppress default logging

if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 19876
    
    # Hello World script - same as Rust integration test
    SCRIPTS = [
        # Response to first inference: Thinking + SendMsg + Idle
        (
            "{ACTION_TOKEN}-thinking\n"
            "User says hello, I should respond.\n"
            "\n"
            "{ACTION_TOKEN}-send_msg\n"
            "user1\n"
            "Hello from the mock test!\n"
            "\n"
            "{ACTION_TOKEN}-idle\n"
        ),
    ]
    
    print(f"[MOCK-LLM] Starting on port {port} with {len(SCRIPTS)} scripts")
    print(f"[MOCK-LLM] Model string: http://127.0.0.1:{port}/v1/chat/completions@test-model")
    server = HTTPServer(('127.0.0.1', port), MockHandler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\n[MOCK-LLM] Stopped")
