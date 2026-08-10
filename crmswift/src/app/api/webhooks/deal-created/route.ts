import { NextRequest, NextResponse } from "next/server";
import { createClient } from "@/lib/supabase/server";

export async function POST(request: NextRequest) {
  try {
    const body = await request.json();
    const { company, value, stage, tenant_id } = body;

    const supabase = createClient();

    // Create deal
    const { data: deal, error } = await supabase
      .from("crm_deals")
      .insert({
        tenant_id,
        title: company,
        value: parseFloat(value) || 0,
        company_name: company,
        status: "open",
      })
      .select()
      .single();

    if (error) throw error;

    // Create webhook event for n8n
    await supabase.rpc("create_webhook_event", {
      p_tenant_id: tenant_id,
      p_event_type: "deal.created",
      p_entity_type: "deal",
      p_entity_id: deal.id,
      p_payload: {
        deal_id: deal.id,
        company,
        value,
        stage,
        timestamp: new Date().toISOString(),
      },
    });

    return NextResponse.json({ success: true, deal });
  } catch (error: any) {
    console.error("Error creating deal:", error);
    return NextResponse.json(
      { success: false, error: error.message },
      { status: 500 }
    );
  }
}
