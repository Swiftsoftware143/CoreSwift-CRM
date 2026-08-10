import { NextRequest, NextResponse } from "next/server";

const API_BASE = process.env.CORESWIFT_API_URL || "http://localhost:8084/api";

export async function GET(req: NextRequest) {
  return proxy(req, "GET");
}

export async function POST(req: NextRequest) {
  return proxy(req, "POST");
}

export async function PUT(req: NextRequest) {
  return proxy(req, "PUT");
}

export async function DELETE(req: NextRequest) {
  return proxy(req, "DELETE");
}

async function proxy(req: NextRequest, method: string) {
  try {
    const path = req.nextUrl.pathname.replace("/api/portfolio", "/portfolio");
    const search = req.nextUrl.search;
    const url = `${API_BASE}${path}${search}`;

    const headers: Record<string, string> = {};
    req.headers.forEach((value, key) => {
      if (!["host", "connection"].includes(key.toLowerCase())) {
        headers[key] = value;
      }
    });
    headers["Content-Type"] = "application/json";

    let body: string | undefined;
    if (method !== "GET" && method !== "DELETE") {
      body = await req.text();
    }

    const res = await fetch(url, {
      method,
      headers,
      body,
    });

    const data = await res.json();
    return NextResponse.json(data, { status: res.status });
  } catch (e: any) {
    return NextResponse.json({ error: e.message || "Proxy error" }, { status: 500 });
  }
}
