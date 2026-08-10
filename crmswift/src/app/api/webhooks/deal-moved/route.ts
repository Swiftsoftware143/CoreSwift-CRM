import { NextRequest, NextResponse } from "next/server";
import { createClient } from "@/lib/supabase/server";

export async function POST(request: NextRequest) {
  try {
    const body = await request.json();
    const { dealId, newStage, tenant_id } = body;

    const supabase = createClient();

    // Update deal stage
    const { data: deal, error } = await supabase
      .from("crm_deals")
      .update({ stage_id: newStage, updated_at: new Date().toISOString() })
      .eq("id", dealId)
      .select()
      .single();

    if (error) throw error;

    // Webhook event is created by database trigger

    return NextResponse.json({ success: true, deal });
  } catch (error: any) {
    console.error("Error moving deal:", error);
    return NextResponse.json(
      { success: false, error: error.message },
      { status: 500 }
    );
  }
}
