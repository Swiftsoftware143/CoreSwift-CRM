import { NextRequest, NextResponse } from "next/server";
import { createClient } from "@/lib/supabase/server";

export async function POST(request: NextRequest) {
  try {
    const body = await request.json();
    const { dealId, date, tenant_id } = body;

    const supabase = createClient();

    // Update deal with follow-up
    const { data: deal, error } = await supabase
      .from("crm_deals")
      .update({
        follow_up_at: date,
        updated_at: new Date().toISOString(),
      })
      .eq("id", dealId)
      .select()
      .single();

    if (error) throw error;

    // Create webhook event for n8n
    await supabase.rpc("create_webhook_event", {
      p_tenant_id: tenant_id,
      p_event_type: "deal.follow_up_set",
      p_entity_type: "deal",
      p_entity_id: dealId,
      p_payload: {
        deal_id: dealId,
        follow_up_at: date,
        timestamp: new Date().toISOString(),
      },
    });

    return NextResponse.json({ success: true, deal });
  } catch (error: any) {
    console.error("Error setting follow-up:", error);
    return NextResponse.json(
      { success: false, error: error.message },
      { status: 500 }
    );
  }
}
