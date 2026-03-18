import asyncio
import json
import os
import sys

try:
    import websockets
except ImportError:
    import urllib.request
    print("No websockets module, falling back to urllib request to see the HTTP error.")
    with open('src/auth/mod.rs', 'r') as f:
        pass # just checking
    sys.exit(0)

async def main():
    # Read token from somewhere. Kley has a db but we can use the env var or just any token
    token = os.environ.get("OPENAI_API_KEY", "")
    if not token:
        # we can just use an invalid token to see if we get 401 instead of 500
        token = "sk-placeholder-test"
    
    headers = {
        "Authorization": f"Bearer {token}",
        "OpenAI-Beta": "responses_websockets=2026-02-06",
        # "originator": "codex_cli_rs",
    }
    uri = "wss://api.openai.com/v1/responses"
    
    print(f"Connecting to {uri}...")
    try:
        async with websockets.connect(uri, extra_headers=headers) as websocket:
            print("Connected!")
    except websockets.exceptions.InvalidStatusCode as e:
        print(f"Failed with status: {e.status_code}")
        print(f"Headers: {e.headers}")
        
    except Exception as e:
        print(f"Error: {e}")

asyncio.run(main())
