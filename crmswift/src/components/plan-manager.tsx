"use client";

import { useState } from "react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Badge } from "@/components/ui/badge";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import {
  AVAILABLE_FEATURES,
  DEFAULT_PLANS,
  PlanConfig,
} from "@/lib/plans-config";
import { Plus, Edit, Trash, Check } from "lucide-react";

// Admin component to manage plans
export function PlanManager() {
  const [plans, setPlans] = useState<PlanConfig[]>(DEFAULT_PLANS);
  const [editingPlan, setEditingPlan] = useState<PlanConfig | null>(null);

  const handleSavePlan = (plan: PlanConfig) => {
    if (editingPlan) {
      setPlans(plans.map((p) => (p.id === plan.id ? plan : p)));
    } else {
      setPlans([...plans, { ...plan, id: `plan_${Date.now()}` }]);
    }
    setEditingPlan(null);
  };

  const handleDeletePlan = (planId: string) => {
    setPlans(plans.filter((p) => p.id !== planId));
  };

  return (
    <div className="space-y-6">
      <div className="flex justify-between items-center">
        <h2 className="text-2xl font-bold">Plan Configuration</h2>
        <Dialog>
          <DialogTrigger asChild>
            <Button className="gap-2">
              <Plus className="w-4 h-4" />
              Add Plan
            </Button>
          </DialogTrigger>
          <DialogContent className="max-w-2xl max-h-[90vh] overflow-y-auto">
            <DialogHeader>
              <DialogTitle>Create New Plan</DialogTitle>
            </DialogHeader>
            <PlanForm onSave={handleSavePlan} />
          </DialogContent>
        </Dialog>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
        {plans.map((plan) => (
          <Card key={plan.id}>
            <CardHeader>
              <div className="flex justify-between items-start">
                <div>
                  <CardTitle>{plan.name}</CardTitle>
                  <p className="text-2xl font-bold mt-2">
                    ${plan.price}
                    <span className="text-sm font-normal text-muted-foreground">
                      /{plan.billing}
                    </span>
                  </p>
                </div>
                <div className="flex gap-1">
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => setEditingPlan(plan)}
                  >
                    <Edit className="w-4 h-4" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => handleDeletePlan(plan.id)}
                  >
                    <Trash className="w-4 h-4" />
                  </Button>
                </div>
              </div>
            </CardHeader>
            <CardContent>
              <div className="space-y-2">
                {AVAILABLE_FEATURES.map((feature) => {
                  const value = plan.features[feature.id];
                  return (
                    <div
                      key={feature.id}
                      className="flex justify-between items-center text-sm"
                    >
                      <span className="text-muted-foreground">
                        {feature.name}
                      </span>
                      <span className="font-medium">
                        {typeof value === "boolean" ? (
                          value ? (
                            <Check className="w-4 h-4 text-green-500" />
                          ) : (
                            <span className="text-muted-foreground">—</span>
                          )
                        ) : (
                          value.toLocaleString()
                        )}
                      </span>
                    </div>
                  );
                })}
              </div>
            </CardContent>
          </Card>
        ))}
      </div>

      {editingPlan && (
        <Dialog open onOpenChange={() => setEditingPlan(null)}>
          <DialogContent className="max-w-2xl max-h-[90vh] overflow-y-auto">
            <DialogHeader>
              <DialogTitle>Edit Plan: {editingPlan.name}</DialogTitle>
            </DialogHeader>
            <PlanForm plan={editingPlan} onSave={handleSavePlan} />
          </DialogContent>
        </Dialog>
      )}
    </div>
  );
}

// Form for creating/editing plans
function PlanForm({
  plan,
  onSave,
}: {
  plan?: PlanConfig;
  onSave: (plan: PlanConfig) => void;
}) {
  const [formData, setFormData] = useState<PlanConfig>(
    plan || {
      id: "",
      name: "",
      price: 29,
      billing: "monthly",
      features: {},
    }
  );

  const handleFeatureChange = (featureId: string, value: boolean | number) => {
    setFormData({
      ...formData,
      features: { ...formData.features, [featureId]: value },
    });
  };

  const handleSubmit = () => {
    onSave(formData);
  };

  return (
    <div className="space-y-6">
      <div className="grid grid-cols-2 gap-4">
        <div className="space-y-2">
          <Label>Plan Name</Label>
          <Input
            value={formData.name}
            onChange={(e) => setFormData({ ...formData, name: e.target.value })}
            placeholder="e.g., Professional"
          />
        </div>
        <div className="space-y-2">
          <Label>Price ($)</Label>
          <Input
            type="number"
            value={formData.price}
            onChange={(e) =>
              setFormData({ ...formData, price: parseInt(e.target.value) })
            }
          />
        </div>
      </div>

      <div className="space-y-4">
        <h3 className="font-semibold">Features</h3>
        {AVAILABLE_FEATURES.map((feature) => {
          const currentValue = formData.features[feature.id] ?? feature.defaultValue;
          return (
            <div
              key={feature.id}
              className="flex items-center justify-between py-2 border-b"
            >
              <div>
                <p className="font-medium">{feature.name}</p>
                <p className="text-sm text-muted-foreground">
                  {feature.description}
                </p>
              </div>
              {typeof feature.defaultValue === "boolean" ? (
                <Switch
                  checked={currentValue as boolean}
                  onCheckedChange={(checked) =>
                    handleFeatureChange(feature.id, checked)
                  }
                />
              ) : (
                <Input
                  type="number"
                  className="w-24"
                  value={currentValue as number}
                  onChange={(e) =>
                    handleFeatureChange(feature.id, parseInt(e.target.value))
                  }
                />
              )}
            </div>
          );
        })}
      </div>

      <Button onClick={handleSubmit} className="w-full">
        {plan ? "Save Changes" : "Create Plan"}
      </Button>
    </div>
  );
}

// Component to show current plan status (for users)
export function CurrentPlanBadge({ planId }: { planId: string }) {
  const plan = DEFAULT_PLANS.find((p) => p.id === planId);
  if (!plan) return null;

  return (
    <Badge variant="secondary" className="gap-1">
      {plan.name} Plan
    </Badge>
  );
}
