import { NextRequest, NextResponse } from "next/server";
import { createClient } from "@/lib/supabase/server";

export async function POST(request: NextRequest) {
  try {
    const body = await request.json();
    const { dealId, type, note, tenant_id } = body;

    const supabase = createClient();

    // Create activity
    const { data: activity, error } = await supabase
      .from("crm_activities")
      .insert({
        tenant_id,
        deal_id: dealId,
        activity_type: type,
        notes: note,
        completed_at: new Date().toISOString(),
      })
      .select()
      .single();

    if (error) throw error;

    // Update deal's last activity
    await supabase
      .from("crm_deals")
      .update({ updated_at: new Date().toISOString() })
      .eq("id", dealId);

    // Create webhook event for n8n
    await supabase.rpc("create_webhook_event", {
      p_tenant_id: tenant_id,
      p_event_type: "activity.created",
      p_entity_type: "activity",
      p_entity_id: activity.id,
      p_payload: {
        activity_id: activity.id,
        deal_id: dealId,
        type,
        note,
        timestamp: new Date().toISOString(),
      },
    });

    return NextResponse.json({ success: true, activity });
  } catch (error: any) {
    console.error("Error logging activity:", error);
    return NextResponse.json(
      { success: false, error: error.message },
      { status: 500 }
    );
  }
}
