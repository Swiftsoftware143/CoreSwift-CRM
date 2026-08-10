"use client";

import { useState, useEffect } from "react";

interface PortfolioCompany {
  id: string;
  name: string;
  slug: string;
  email?: string;
  description?: string;
  is_active?: boolean;
  created_at?: string;
}

export default function PortfolioPage() {
  const [companies, setCompanies] = useState<PortfolioCompany[]>([]);
  const [showModal, setShowModal] = useState(false);
  const [editItem, setEditItem] = useState<PortfolioCompany | null>(null);
  const [search, setSearch] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");

  const load = async () => {
    try {
      const token = localStorage.getItem("cs_token");
      const res = await fetch("/api/portfolio", {
        headers: token ? { Authorization: `Bearer ${token}` } : {},
      });
      const data = await res.json();
      setCompanies(data.portfolio_companies || data || []);
    } catch (e) {
      console.error(e);
      setError("Failed to load portfolio companies");
    }
    setLoading(false);
  };

  useEffect(() => { load(); }, []);

  const handleSave = async (data: Partial<PortfolioCompany>) => {
    const token = localStorage.getItem("cs_token");
    const headers = { "Content-Type": "application/json", ...(token ? { Authorization: `Bearer ${token}` } as any : {}) };
    const url = editItem ? `/api/portfolio/${editItem.id}` : "/api/portfolio";
    const method = editItem ? "PUT" : "POST";
    try {
      const res = await fetch(url, { method, headers, body: JSON.stringify(data) });
      if (!res.ok) throw new Error((await res.json()).error || "Failed");
      setShowModal(false);
      load();
    } catch (e: any) {
      alert(e.message);
    }
  };

  const handleDelete = async (id: string) => {
    if (!confirm("Delete this portfolio company?")) return;
    const token = localStorage.getItem("cs_token");
    try {
      const res = await fetch(`/api/portfolio/${id}`, {
        method: "DELETE",
        headers: token ? { Authorization: `Bearer ${token}` } : {},
      });
      if (!res.ok) throw new Error((await res.json()).error || "Failed");
      load();
    } catch (e: any) {
      alert(e.message);
    }
  };

  const handleImpersonate = async (company: PortfolioCompany) => {
    if (!confirm(`Impersonate as ${company.name}?`)) return;
    const token = localStorage.getItem("cs_token");
    try {
      const res = await fetch("/api/admin/impersonate", {
        method: "POST",
        headers: { "Content-Type": "application/json", ...(token ? { Authorization: `Bearer ${token}` } as any : {}) },
        body: JSON.stringify({ account_id: company.id }),
      });
      if (!res.ok) throw new Error((await res.json()).error || "Failed");
      const data = await res.json();
      sessionStorage.setItem("impersonate_token", data.token);
      sessionStorage.setItem("impersonate_company", JSON.stringify(company));
      window.open("https://coreswiftcrm.com", "_blank");
    } catch (e: any) {
      alert("Impersonation failed: " + e.message);
    }
  };

  const filtered = companies.filter(
    (c) =>
      !search ||
      (c.name || "").toLowerCase().includes(search.toLowerCase()) ||
      (c.slug || "").toLowerCase().includes(search.toLowerCase()) ||
      (c.email || "").toLowerCase().includes(search.toLowerCase())
  );

  const [formName, setFormName] = useState("");
  const [formSlug, setFormSlug] = useState("");
  const [formEmail, setFormEmail] = useState("");

  const openModal = (item: PortfolioCompany | null) => {
    setEditItem(item);
    setFormName(item?.name || "");
    setFormSlug(item?.slug || "");
    setFormEmail(item?.email || "");
    setShowModal(true);
  };

  if (loading) return <div className="p-8 text-gray-500">Loading...</div>;
  if (error) return <div className="p-8 text-red-500">{error}</div>;

  return (
    <div className="p-6 max-w-6xl mx-auto">
      <div className="flex items-center justify-between mb-6">
        <h1 className="text-2xl font-bold text-gray-800">Portfolio Companies</h1>
        <div className="flex gap-3 items-center">
          <input
            className="border border-gray-300 rounded-lg px-3 py-2 text-sm w-64 focus:outline-none focus:border-blue-500"
            placeholder="Search..."
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
          <button
            className="bg-green-600 text-white px-4 py-2 rounded-lg text-sm font-medium hover:bg-green-700"
            onClick={() => openModal(null)}
          >
            + Add Company
          </button>
        </div>
      </div>

      <div className="bg-white rounded-xl shadow-sm overflow-hidden">
        <table className="w-full">
          <thead>
            <tr className="border-b-2 border-gray-200">
              <th className="text-left px-4 py-3 text-xs font-semibold text-gray-500 uppercase">Name</th>
              <th className="text-left px-4 py-3 text-xs font-semibold text-gray-500 uppercase">Email</th>
              <th className="text-left px-4 py-3 text-xs font-semibold text-gray-500 uppercase">Slug</th>
              <th className="text-left px-4 py-3 text-xs font-semibold text-gray-500 uppercase">Active</th>
              <th className="text-left px-4 py-3 text-xs font-semibold text-gray-500 uppercase">Actions</th>
            </tr>
          </thead>
          <tbody>
            {filtered.map((c) => (
              <tr key={c.id} className="border-b border-gray-50 hover:bg-gray-50">
                <td className="px-4 py-3 font-medium text-gray-800">{c.name}</td>
                <td className="px-4 py-3 text-gray-500">{c.email || "-"}</td>
                <td className="px-4 py-3 text-gray-500">{c.slug || "-"}</td>
                <td className="px-4 py-3">
                  {c.is_active !== false ? (
                    <span className="inline-block px-2 py-1 text-xs font-medium bg-green-100 text-green-700 rounded-full">Active</span>
                  ) : (
                    <span className="inline-block px-2 py-1 text-xs font-medium bg-red-100 text-red-700 rounded-full">Inactive</span>
                  )}
                </td>
                <td className="px-4 py-3 flex gap-2">
                  <button
                    className="bg-purple-600 text-white px-3 py-1.5 rounded-lg text-xs font-medium hover:bg-purple-700"
                    onClick={() => handleImpersonate(c)}
                  >
                    🔑 Impersonate
                  </button>
                  <button
                    className="border border-gray-300 text-gray-600 px-3 py-1.5 rounded-lg text-xs font-medium hover:bg-gray-100"
                    onClick={() => openModal(c)}
                  >
                    Edit
                  </button>
                  <button
                    className="bg-red-600 text-white px-3 py-1.5 rounded-lg text-xs font-medium hover:bg-red-700"
                    onClick={() => handleDelete(c.id)}
                  >
                    Delete
                  </button>
                </td>
              </tr>
            ))}
            {filtered.length === 0 && (
              <tr>
                <td colSpan={5} className="text-center text-gray-400 py-8">
                  No portfolio companies yet.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>

      {showModal && (
        <div
          className="fixed inset-0 bg-black/50 flex items-center justify-center z-50"
          onClick={(e) => e.target === e.currentTarget && setShowModal(false)}
        >
          <div className="bg-white rounded-2xl p-6 w-full max-w-md shadow-xl">
            <h3 className="text-lg font-bold mb-4">
              {editItem ? "Edit" : "Add"} Portfolio Company
            </h3>
            <div className="space-y-3">
              <div>
                <label className="text-sm font-medium text-gray-600 block mb-1">Company Name</label>
                <input className="border border-gray-300 rounded-lg px-3 py-2 w-full text-sm" value={formName} onChange={(e) => setFormName(e.target.value)} />
              </div>
              <div>
                <label className="text-sm font-medium text-gray-600 block mb-1">Email</label>
                <input type="email" className="border border-gray-300 rounded-lg px-3 py-2 w-full text-sm" value={formEmail} onChange={(e) => setFormEmail(e.target.value)} />
              </div>
              <div>
                <label className="text-sm font-medium text-gray-600 block mb-1">Slug</label>
                <input className="border border-gray-300 rounded-lg px-3 py-2 w-full text-sm" value={formSlug} onChange={(e) => setFormSlug(e.target.value)} placeholder="my-company" />
              </div>
              <div className="flex gap-2 justify-end mt-4">
                <button className="border border-gray-300 text-gray-600 px-4 py-2 rounded-lg text-sm hover:bg-gray-100" onClick={() => setShowModal(false)}>Cancel</button>
                <button className="bg-blue-600 text-white px-4 py-2 rounded-lg text-sm font-medium hover:bg-blue-700" onClick={() => handleSave({ name: formName, slug: formSlug, email: formEmail })}>
                  {editItem ? "Save" : "Add"}
                </button>
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
