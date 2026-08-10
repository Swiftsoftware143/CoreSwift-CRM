"use client";

import { ReactNode } from "react";
import { hasFeature, getFeatureLimit } from "@/lib/plans-config";
import { Card, CardContent } from "@/components/ui/card";
import { Lock, AlertCircle } from "lucide-react";
import { Button } from "@/components/ui/button";

interface FeatureGateProps {
  featureId: string;
  requiredValue?: number;
  planFeatures: Record<string, boolean | number>;
  children: ReactNode;
  fallback?: ReactNode;
}

// Gate content based on plan features
export function FeatureGate({
  featureId,
  requiredValue,
  planFeatures,
  children,
  fallback,
}: FeatureGateProps) {
  const hasAccess = hasFeature(planFeatures, featureId, requiredValue);

  if (hasAccess) {
    return <>{children}</>;
  }

  if (fallback) {
    return <>{fallback}</>;
  }

  return (
    <Card className="opacity-50">
      <CardContent className="flex flex-col items-center justify-center py-12 text-center">
        <Lock className="h-12 w-12 text-muted-foreground mb-4" />
        <h3 className="font-semibold text-lg">Upgrade Required</h3>
        <p className="text-muted-foreground mt-2">
          This feature is not available on your current plan.
        </p>
        <Button className="mt-4" variant="outline">
          View Plans
        </Button>
      </CardContent>
    </Card>
  );
}

// Show usage counter (e.g., "45/100 contacts used")
interface UsageCounterProps {
  featureId: string;
  currentUsage: number;
  planFeatures: Record<string, boolean | number>;
}

export function UsageCounter({
  featureId,
  currentUsage,
  planFeatures,
}: UsageCounterProps) {
  const limit = getFeatureLimit(planFeatures, featureId);
  const percentage = Math.min((currentUsage / limit) * 100, 100);

  return (
    <div className="space-y-2">
      <div className="flex justify-between text-sm">
        <span className="text-muted-foreground">
          {currentUsage.toLocaleString()} / {limit.toLocaleString()}
        </span>
        {percentage > 80 && (
          <span className="text-orange-500 flex items-center gap-1">
            <AlertCircle className="w-4 h-4" />
            {percentage >= 100 ? "Limit reached" : "Approaching limit"}
          </span>
        )}
      </div>
      <div className="h-2 bg-muted rounded-full overflow-hidden">
        <div
          className={`h-full transition-all ${
            percentage >= 100
              ? "bg-red-500"
              : percentage > 80
              ? "bg-orange-500"
              : "bg-green-500"
          }`}
          style={{ width: `${percentage}%` }}
        />
      </div>
    </div>
  );
}

// Hook to check features in components
export function usePlanFeatures(planFeatures: Record<string, boolean | number>) {
  return {
    can: (featureId: string, value?: number) =>
      hasFeature(planFeatures, featureId, value),
    limit: (featureId: string) => getFeatureLimit(planFeatures, featureId),
    features: planFeatures,
  };
}
