import { PipelineBoard } from "@/components/pipeline-board";
import { QuickAddDeal } from "@/components/quick-actions";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { AlertCircle, TrendingUp, Users, DollarSign } from "lucide-react";

// Dashboard stats component
function DashboardStats() {
  return (
    <div className="grid grid-cols-4 gap-4 mb-6">
      <Card>
        <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
          <CardTitle className="text-sm font-medium">Pipeline Value</CardTitle>
          <DollarSign className="h-4 w-4 text-muted-foreground" />
        </CardHeader>
        <CardContent>
          <div className="text-2xl font-bold">$25,000</div>
          <p className="text-xs text-muted-foreground">+12% from last month</p>
        </CardContent>
      </Card>
      <Card>
        <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
          <CardTitle className="text-sm font-medium">Active Deals</CardTitle>
          <TrendingUp className="h-4 w-4 text-muted-foreground" />
        </CardHeader>
        <CardContent>
          <div className="text-2xl font-bold">12</div>
          <p className="text-xs text-muted-foreground">3 new this week</p>
        </CardContent>
      </Card>
      <Card>
        <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
          <CardTitle className="text-sm font-medium">Contacts</CardTitle>
          <Users className="h-4 w-4 text-muted-foreground" />
        </CardHeader>
        <CardContent>
          <div className="text-2xl font-bold">48</div>
          <p className="text-xs text-muted-foreground">+8 this month</p>
        </CardContent>
      </Card>
      <Card>
        <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
          <CardTitle className="text-sm font-medium">Needs Attention</CardTitle>
          <AlertCircle className="h-4 w-4 text-orange-500" />
        </CardHeader>
        <CardContent>
          <div className="text-2xl font-bold text-orange-500">3</div>
          <p className="text-xs text-muted-foreground">Overdue follow-ups</p>
        </CardContent>
      </Card>
    </div>
  );
}

// Overdue follow-ups banner
function AttentionBanner() {
  return (
    <div className="bg-orange-50 border border-orange-200 rounded-lg p-4 mb-6">
      <div className="flex items-center gap-2">
        <AlertCircle className="h-5 w-5 text-orange-500" />
        <h3 className="font-semibold text-orange-800">Needs Attention Today</h3>
      </div>
      <div className="mt-2 space-y-1">
        <p className="text-sm text-orange-700">
          • Acme Corp - Follow-up overdue 2 days
        </p>
        <p className="text-sm text-orange-700">
          • TechStart Inc - Proposal due today
        </p>
        <p className="text-sm text-orange-700">
          • Beta Solutions - Call scheduled 3pm
        </p>
      </div>
    </div>
  );
}

export default function Home() {
  return (
    <main className="min-h-screen p-6">
      <div className="max-w-7xl mx-auto">
        {/* Header */}
        <div className="flex justify-between items-center mb-6">
          <div>
            <h1 className="text-3xl font-bold">CRMSwift</h1>
            <p className="text-muted-foreground">Your pipeline at a glance</p>
          </div>
          <QuickAddDeal />
        </div>

        {/* Attention Banner */}
        <AttentionBanner />

        {/* Stats */}
        <DashboardStats />

        {/* Pipeline */}
        <div className="mt-6">
          <h2 className="text-lg font-semibold mb-4">Sales Pipeline</h2>
          <PipelineBoard />
        </div>
      </div>
    </main>
  );
}
