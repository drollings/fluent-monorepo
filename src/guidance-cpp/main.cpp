// guidance-cpp: C++ AST provider for the guidance system
// Generates .guidance/src/**/*.{cpp,c,cc,h,hpp}.json compatible with .guidance/schema.json
//
// Usage:
//   guidance-cpp sync --scan <dir> --output <dir> [--infill] [--regen] [--debug]
//   guidance-cpp sync --file <path> --output <dir> [--infill] [--regen] [--debug]
//   guidance-cpp scrub --scan <dir>

#include <boost/json.hpp>

#include <cppparser/cppparser.h>
#include <cppast/cppast.h>
#include <cppast/cpp_entity_info_accessor.h>
#include <cppast/cpp_entity_type.h>
#include <cppast/cpp_compound.h>
#include <cppast/cpp_function.h>
#include <cppast/cpp_var.h>
#include <cppast/cpp_var_type.h>
#include <cppast/cpp_enum.h>
#include <cppast/cpp_documentation_comment.h>
#include <cppast/cpp_entity_access_speciifier.h>
#include <cppast/cppconst.h>

#include <openssl/sha.h>

#include <algorithm>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <optional>
#include <set>
#include <sstream>
#include <string>
#include <vector>
#include <chrono>

namespace fs = std::filesystem;
namespace json = boost::json;
using namespace cppast;

// ---------------------------------------------------------------------------
// Utility: SHA-256 hex digest
// ---------------------------------------------------------------------------
static std::string sha256hex(const std::string& s) {
    unsigned char hash[SHA256_DIGEST_LENGTH];
    SHA256(reinterpret_cast<const unsigned char*>(s.c_str()), s.size(), hash);
    char hex[65];
    hex[64] = '\0';
    for (int i = 0; i < SHA256_DIGEST_LENGTH; ++i)
        std::snprintf(hex + 2 * i, 3, "%02x", hash[i]);
    return {hex, 64};
}

// ---------------------------------------------------------------------------
// Utility: trim whitespace
// ---------------------------------------------------------------------------
static std::string trim(const std::string& s) {
    const char* ws = " \t\r\n";
    auto b = s.find_first_not_of(ws);
    if (b == std::string::npos) return {};
    auto e = s.find_last_not_of(ws);
    return s.substr(b, e - b + 1);
}

// ---------------------------------------------------------------------------
// Type rendering
// ---------------------------------------------------------------------------
static std::string renderVarType(const CppVarType& vt) {
    std::string s;
    if (vt.typeAttr() & CppIdentifierAttrib::CONST) s += "const ";
    s += vt.baseType();
    const auto& mod = vt.typeModifier();
    for (int i = 0; i < mod.ptrLevel_; ++i) s += "*";
    if      (mod.refType_ == CppRefType::BY_REF)   s += "&";
    else if (mod.refType_ == CppRefType::RVAL_REF)  s += "&&";
    return trim(s);
}

// Collect parameters from a function-like entity
struct Param {
    std::string name;
    std::string type;
};

static std::vector<Param> collectParams(const CppFuncOrCtorCommon& fn) {
    std::vector<Param> result;
    fn.visitParams([&](const CppEntity& entity) {
        if (entity.entityType() == CppEntityType::VAR) {
            const auto& var = static_cast<const CppVar&>(entity);
            result.push_back({var.name(), renderVarType(var.varType())});
        }
        return true;
    });
    return result;
}

// Param list as "name:type,name:type" for match_hash
static std::string paramHashStr(const std::vector<Param>& params) {
    std::string r;
    bool first = true;
    for (const auto& p : params) {
        if (!first) r += ',';
        first = false;
        r += p.name + ':' + p.type;
    }
    return r;
}

// Human-readable param list: "(Type name, ...)"
static std::string paramSigStr(const std::vector<Param>& params, bool hasSelf = false) {
    std::string r = "(";
    bool first = true;
    if (hasSelf) { r += "self"; first = false; }
    for (const auto& p : params) {
        if (!first) r += ", ";
        first = false;
        r += p.type + " " + p.name;
    }
    r += ")";
    return r;
}

// JSON array of param objects
static json::array paramArray(const std::vector<Param>& params) {
    json::array arr;
    for (const auto& p : params) {
        json::object obj;
        obj["name"]    = p.name;
        obj["type"]    = p.type;
        obj["default"] = nullptr;
        arr.push_back(obj);
    }
    return arr;
}

// ---------------------------------------------------------------------------
// Match hash computation
// ---------------------------------------------------------------------------
static std::string functionMatchHash(const std::string& name,
                                     const std::vector<Param>& params,
                                     const std::string& returnType) {
    return sha256hex(name + "(" + paramHashStr(params) + ")->" + returnType);
}

static std::string compoundMatchHash(const std::string& kind,
                                     const std::string& name,
                                     const std::list<CppInheritanceInfo>& bases) {
    std::string sig = kind + " " + name + "(";
    bool first = true;
    for (const auto& b : bases) {
        if (!first) sig += ",";
        first = false;
        sig += b.baseName;
    }
    sig += ")";
    return sha256hex(sig);
}

// ---------------------------------------------------------------------------
// Strip doc comment markers → plain text
// ---------------------------------------------------------------------------
static std::string cleanDocComment(const std::string& raw) {
    std::istringstream ss(raw);
    std::string line, result;
    bool first = true;
    while (std::getline(ss, line)) {
        std::string t = trim(line);
        if (t.size() >= 3 && t.substr(0, 3) == "///") t = trim(t.substr(3));
        else if (t.size() >= 2 && t.substr(0, 2) == "//") t = trim(t.substr(2));
        else if (t.size() >= 2 && t.substr(0, 2) == "/*") t = trim(t.substr(2));
        else if (t.size() >= 2 && t.substr(0, 2) == "*/") t = trim(t.substr(2));
        else if (!t.empty() && t[0] == '*') t = trim(t.substr(1));
        if (!t.empty()) {
            if (!first) result += "\n";
            result += t;
            first = false;
        }
    }
    return result;
}

// ---------------------------------------------------------------------------
// Compound type → string keyword
// ---------------------------------------------------------------------------
static std::string compoundKind(CppCompoundType ct) {
    switch (ct) {
        case CppCompoundType::CLASS:  return "class";
        case CppCompoundType::STRUCT: return "struct";
        case CppCompoundType::UNION:  return "union";
        default: return "struct";
    }
}

// ---------------------------------------------------------------------------
// Empty member template
// ---------------------------------------------------------------------------
static json::object emptyMember() {
    json::object m;
    m["line"]         = nullptr;
    m["comment"]      = nullptr;
    m["signature"]    = nullptr;
    m["params"]       = json::array{};
    m["returns"]      = nullptr;
    m["patterns"]     = json::array{};
    m["tags"]         = json::array{};
    m["is_pub"]       = true;
    m["members"]      = json::array{};
    m["skills"]       = json::array{};
    m["capabilities"] = json::array{};
    m["equivalents"]  = json::array{};
    return m;
}

// Forward declaration
static json::array processMembers(const CppCompound& compound, bool inClass, bool debug);

// ---------------------------------------------------------------------------
// Process a CppFunction entity
// ---------------------------------------------------------------------------
static json::object processFunction(const CppFunction& fn, bool inClass, bool isPublic) {
    auto m = emptyMember();
    m["name"] = fn.name();
    m["is_pub"] = isPublic;

    const auto params = collectParams(fn);
    std::string returnType = fn.returnType() ? renderVarType(*fn.returnType()) : "void";

    m["type"]       = inClass
                        ? (isPublic ? "method" : "method_private")
                        : (isPublic ? "fn_decl" : "fn_private");
    m["signature"]  = std::string("fn ") + fn.name() + paramSigStr(params, inClass) + " -> " + returnType;
    m["match_hash"] = functionMatchHash(fn.name(), params, returnType);
    m["params"]     = paramArray(params);
    m["returns"]    = returnType;
    return m;
}

// ---------------------------------------------------------------------------
// Process a CppConstructor entity
// ---------------------------------------------------------------------------
static json::object processConstructor(const CppConstructor& ctor, bool isPublic) {
    auto m = emptyMember();
    m["name"]    = ctor.name();
    m["is_pub"]  = isPublic;
    m["type"]    = isPublic ? "method" : "method_private";

    const auto params = collectParams(ctor);
    m["signature"]  = std::string("fn ") + ctor.name() + paramSigStr(params, true) + " -> void";
    m["match_hash"] = functionMatchHash(ctor.name(), params, "void");
    m["params"]     = paramArray(params);
    m["returns"]    = "void";
    return m;
}

// ---------------------------------------------------------------------------
// Process a CppDestructor entity
// ---------------------------------------------------------------------------
static json::object processDestructor(const CppDestructor& dtor, bool isPublic) {
    auto m = emptyMember();
    m["name"]       = dtor.name();
    m["is_pub"]     = isPublic;
    m["type"]       = isPublic ? "method" : "method_private";
    m["signature"]  = std::string("fn ") + dtor.name() + "(self) -> void";
    m["match_hash"] = sha256hex(dtor.name() + "()->void");
    m["returns"]    = "void";
    return m;
}

// ---------------------------------------------------------------------------
// Process CppEnum
// ---------------------------------------------------------------------------
static json::object processEnum(const CppEnum& en, bool isPublic) {
    auto m = emptyMember();
    const std::string name = en.name().empty() ? "(anonymous)" : en.name();
    m["name"]       = name;
    m["type"]       = "enum";
    m["is_pub"]     = isPublic;
    m["signature"]  = "enum " + name;
    m["match_hash"] = sha256hex("enum " + name + "()");

    json::array fields;
    for (const auto& item : en.itemList()) {
        if (item.isNonConstEntity()) continue;
        auto f = emptyMember();
        f["name"]       = item.name();
        f["type"]       = "enum_field";
        f["match_hash"] = sha256hex("enum_field " + item.name());
        fields.push_back(f);
    }
    m["members"] = fields;
    return m;
}

// ---------------------------------------------------------------------------
// Process CppCompound (class/struct/union)
// ---------------------------------------------------------------------------
static json::object processCompound(const CppCompound& compound, bool isPublic, bool debug) {
    const std::string kind = compoundKind(compound.compoundType());
    const std::string name = compound.name();

    auto m = emptyMember();
    m["name"]    = name.empty() ? "(anonymous)" : name;
    m["type"]    = "struct";
    m["is_pub"]  = isPublic;

    const auto& bases = compound.inheritanceList();
    std::string baseSig;
    {
        bool first = true;
        for (const auto& b : bases) {
            if (!first) baseSig += ", ";
            first = false;
            baseSig += b.baseName;
        }
    }
    std::string sig = kind + " " + name;
    if (!bases.empty()) sig += "(" + baseSig + ")";
    m["signature"]  = sig;
    m["match_hash"] = compoundMatchHash(kind, name, bases);
    m["members"]    = processMembers(compound, /*inClass=*/true, debug);
    return m;
}

// ---------------------------------------------------------------------------
// Walk compound's direct children and emit member objects
// ---------------------------------------------------------------------------
static json::array processMembers(const CppCompound& compound, bool inClass, bool debug) {
    json::array members;

    CppAccessType currentAccess = (compound.compoundType() == CppCompoundType::CLASS)
        ? CppAccessType::PRIVATE
        : CppAccessType::PUBLIC;

    std::optional<std::string> lastComment;

    compound.visitAll([&](const CppEntity& entity) -> bool {
        const auto etype = entity.entityType();

        // Track doc comments to attach to the next entity
        if (etype == CppEntityType::DOCUMENTATION_COMMENT) {
            const auto& doc = static_cast<const CppDocumentationComment&>(entity);
            lastComment = cleanDocComment(doc.str());
            return true;
        }

        // Track access specifiers
        if (etype == CppEntityType::ENTITY_ACCESS_SPECIFIER) {
            const auto& acc = static_cast<const CppEntityAccessSpecifier&>(entity);
            currentAccess = acc.type();
            lastComment.reset();
            return true;
        }

        const bool isPublic = (currentAccess == CppAccessType::PUBLIC);
        std::optional<json::object> member;

        if (etype == CppEntityType::FUNCTION) {
            const auto& fn = static_cast<const CppFunction&>(entity);
            member = processFunction(fn, inClass, isPublic);
        } else if (etype == CppEntityType::CONSTRUCTOR) {
            const auto& ctor = static_cast<const CppConstructor&>(entity);
            member = processConstructor(ctor, isPublic);
        } else if (etype == CppEntityType::DESTRUCTOR) {
            const auto& dtor = static_cast<const CppDestructor&>(entity);
            member = processDestructor(dtor, isPublic);
        } else if (etype == CppEntityType::ENUM) {
            const auto& en = static_cast<const CppEnum&>(entity);
            if (!en.name().empty())
                member = processEnum(en, isPublic);
        } else if (etype == CppEntityType::COMPOUND) {
            const auto& sub = static_cast<const CppCompound&>(entity);
            const auto ct = sub.compoundType();
            if (ct == CppCompoundType::CLASS ||
                ct == CppCompoundType::STRUCT ||
                ct == CppCompoundType::UNION) {
                member = processCompound(sub, isPublic, debug);
            }
        }

        if (member) {
            if (lastComment)
                (*member)["comment"] = *lastComment;
            members.push_back(*member);
            lastComment.reset();
        } else if (etype != CppEntityType::PREPROCESSOR &&
                   etype != CppEntityType::USING_NAMESPACE &&
                   etype != CppEntityType::FORWARD_CLASS_DECL &&
                   etype != CppEntityType::DOCUMENTATION_COMMENT) {
            lastComment.reset();
        }

        return true;
    });

    return members;
}

// ---------------------------------------------------------------------------
// Extract file-level doc comment
// ---------------------------------------------------------------------------
static std::optional<std::string> extractFileComment(const CppCompound& fileAst) {
    std::optional<std::string> comment;
    fileAst.visitAll([&](const CppEntity& entity) -> bool {
        if (entity.entityType() == CppEntityType::DOCUMENTATION_COMMENT) {
            const auto& doc = static_cast<const CppDocumentationComment&>(entity);
            comment = cleanDocComment(doc.str());
            return false;
        }
        if (entity.entityType() != CppEntityType::PREPROCESSOR)
            return false;
        return true;
    });
    return comment;
}

// ---------------------------------------------------------------------------
// Scan for used_by (files that #include this file)
// ---------------------------------------------------------------------------
static std::vector<std::string> findUsedBy(const fs::path& srcFile,
                                            const fs::path& scanDir) {
    const std::string stem = srcFile.filename().string();
    std::vector<std::string> result;
    static const std::set<std::string> cppExts = {
        ".cpp", ".c", ".cc", ".h", ".hpp", ".cxx", ".hxx"
    };
    std::error_code ec;
    for (const auto& de : fs::recursive_directory_iterator(scanDir, fs::directory_options::skip_permission_denied, ec)) {
        if (!de.is_regular_file()) continue;
        if (cppExts.find(de.path().extension().string()) == cppExts.end()) continue;
        if (de.path() == srcFile) continue;
        std::ifstream f(de.path());
        std::string line;
        while (std::getline(f, line)) {
            if (line.find("#include") != std::string::npos &&
                line.find(stem) != std::string::npos) {
                auto rel = fs::relative(de.path(), scanDir, ec);
                result.push_back(rel.string());
                break;
            }
        }
    }
    return result;
}

// ---------------------------------------------------------------------------
// Find cross-language equivalents (same stem, different language extension)
// ---------------------------------------------------------------------------
static std::vector<std::string> findEquivalents(const fs::path& srcFile,
                                                  const fs::path& scanDir) {
    static const std::set<std::string> otherExts = {
        ".zig", ".py", ".rs", ".go", ".ts", ".js"
    };
    const std::string stem = srcFile.stem().string();
    std::vector<std::string> result;
    std::error_code ec;
    for (const auto& de : fs::recursive_directory_iterator(scanDir, fs::directory_options::skip_permission_denied, ec)) {
        if (!de.is_regular_file()) continue;
        if (de.path().stem().string() != stem) continue;
        if (otherExts.find(de.path().extension().string()) == otherExts.end()) continue;
        result.push_back(fs::relative(de.path(), scanDir, ec).string());
    }
    return result;
}

// ---------------------------------------------------------------------------
// Build module name from relative path (path.to.module)
// ---------------------------------------------------------------------------
static std::string moduleName(const fs::path& rel) {
    fs::path noext = rel;
    noext.replace_extension();
    std::string s = noext.string();
    for (char& c : s)
        if (c == '/' || c == '\\') c = '.';
    return s;
}

// ---------------------------------------------------------------------------
// Incremental check
// ---------------------------------------------------------------------------
static bool needsUpdate(const fs::path& srcPath, const fs::path& jsonPath, bool regen) {
    if (regen) return true;
    std::error_code ec;
    if (!fs::exists(jsonPath, ec)) return true;
    return fs::last_write_time(srcPath, ec) > fs::last_write_time(jsonPath, ec);
}

// ---------------------------------------------------------------------------
// Process one source file → emit JSON
// ---------------------------------------------------------------------------
static bool processFile(const fs::path& srcPath,
                        const fs::path& outputDir,
                        const fs::path& scanDir,
                        bool regen,
                        bool debug) {
    std::error_code ec;
    auto relToScan = fs::relative(srcPath, scanDir, ec);
    fs::path jsonPath = outputDir / "src" / (relToScan.string() + ".json");

    if (!needsUpdate(srcPath, jsonPath, regen)) {
        if (debug) std::cerr << "[skip] " << srcPath << "\n";
        return true;
    }
    if (debug) std::cerr << "[proc] " << srcPath << "\n";

    // Read source
    std::string src;
    {
        std::ifstream f(srcPath);
        if (!f) {
            std::cerr << "[error] Cannot read: " << srcPath << "\n";
            return false;
        }
        std::ostringstream ss;
        ss << f.rdbuf();
        src = ss.str();
    }

    // cppparser requires buffer to end with two null bytes
    src += '\0';
    src += '\0';

    // Parse
    cppparser::CppParser parser;
    parser.parseFunctionBodyAsBlob(true);
    parser.addKnownMacros({
        "Q_OBJECT", "OVERRIDE", "FINAL", "NOEXCEPT", "NODISCARD",
        "DEPRECATED", "FALLTHROUGH", "LIKELY", "UNLIKELY",
        "API_EXPORT", "DLL_EXPORT", "WINAPI", "__stdcall",
        "nullptr_t", "noexcept", "override", "final"
    });
    parser.addKnownApiDecors({
        "__declspec(dllexport)", "__declspec(dllimport)",
        "__attribute__((visibility(\"default\")))"
    });

    auto fileAst = parser.parseStream(src.data(), src.size());
    if (!fileAst) {
        std::cerr << "[error] Parse failed: " << srcPath << "\n";
        return false;
    }

    // Build document
    json::object doc;
    {
        json::object meta;
        meta["module"]   = moduleName(relToScan);
        meta["source"]   = relToScan.string();
        meta["language"] = "cpp";
        doc["meta"] = meta;
    }

    auto fileComment = extractFileComment(*fileAst);
    doc["comment"]      = fileComment ? json::value(*fileComment) : json::value(nullptr);
    doc["detail"]       = nullptr;
    doc["keywords"]     = json::array{};
    doc["skills"]       = json::array{};
    doc["capabilities"] = json::array{};
    doc["hashtags"]     = json::array{};

    {
        json::array ub;
        for (const auto& s : findUsedBy(srcPath, scanDir))
            ub.emplace_back(s);
        doc["used_by"] = ub;
    }

    doc["members"] = processMembers(*fileAst, /*inClass=*/false, debug);

    // Write output
    fs::create_directories(jsonPath.parent_path(), ec);
    {
        std::ofstream out(jsonPath);
        if (!out) {
            std::cerr << "[error] Cannot write: " << jsonPath << "\n";
            return false;
        }
        out << json::serialize(doc) << "\n";
    }

    // Set mtime to now + 1s (validated marker)
    fs::last_write_time(jsonPath,
        fs::file_time_type::clock::now() + std::chrono::seconds(1), ec);

    std::cout << "[ok] " << relToScan.string() << "\n";
    return true;
}

// ---------------------------------------------------------------------------
// Scrub: blank obviously synthetic comments in JSON guidance files
// ---------------------------------------------------------------------------
static void scrubFile(const fs::path& jsonPath, bool debug) {
    std::ifstream in(jsonPath);
    if (!in) return;
    std::string content((std::istreambuf_iterator<char>(in)), {});
    in.close();

    std::error_code ec2;
    json::value v = json::parse(content, ec2);
    if (ec2) return;

    std::function<void(json::value&)> scrub = [&](json::value& val) {
        if (val.is_object()) {
            auto& obj = val.as_object();
            if (auto* c = obj.if_contains("comment")) {
                if (c->is_string()) {
                    const auto& s = c->as_string();
                    if (s.find("TODO") != std::string::npos ||
                        s.find("FIXME") != std::string::npos ||
                        s.find("Generated") != std::string::npos ||
                        s.find("placeholder") != std::string::npos ||
                        s.size() < 4) {
                        obj["comment"] = nullptr;
                    }
                }
            }
            for (auto& [k, v2] : obj) scrub(v2);
        } else if (val.is_array()) {
            for (auto& elem : val.as_array()) scrub(elem);
        }
    };
    scrub(v);

    std::ofstream out(jsonPath);
    if (!out) return;
    out << json::serialize(v) << "\n";
    if (debug) std::cerr << "[scrub] " << jsonPath << "\n";
}

// ---------------------------------------------------------------------------
// CLI entry point
// ---------------------------------------------------------------------------
int main(int argc, char* argv[]) {
    if (argc < 2) {
        std::cerr
            << "Usage:\n"
            << "  guidance-cpp sync --scan <dir> --output <dir> [--regen] [--debug]\n"
            << "  guidance-cpp sync --file <path> --output <dir> [--regen] [--debug]\n"
            << "  guidance-cpp scrub --scan <dir>\n";
        return 1;
    }

    const std::string cmd = argv[1];

    if (cmd == "sync") {
        fs::path scanDir, singleFile, outputDir;
        bool regen = false, debug = false;

        for (int i = 2; i < argc; ++i) {
            std::string a = argv[i];
            if      (a == "--scan"   && i + 1 < argc) scanDir    = argv[++i];
            else if (a == "--file"   && i + 1 < argc) singleFile = argv[++i];
            else if (a == "--output" && i + 1 < argc) outputDir  = argv[++i];
            else if (a == "--regen")  regen = true;
            else if (a == "--debug")  debug = true;
            // --infill: LLM enhancement not implemented
        }

        if (outputDir.empty()) {
            std::cerr << "[error] --output is required\n";
            return 1;
        }

        static const std::set<std::string> cppExts = {
            ".cpp", ".c", ".cc", ".h", ".hpp", ".cxx", ".hxx"
        };

        int ok = 0, fail = 0;

        if (!singleFile.empty()) {
            const fs::path dir = scanDir.empty() ? singleFile.parent_path() : scanDir;
            if (processFile(singleFile, outputDir, dir, regen, debug)) ++ok;
            else ++fail;
        } else if (!scanDir.empty()) {
            std::error_code ec;
            for (const auto& de : fs::recursive_directory_iterator(scanDir, fs::directory_options::skip_permission_denied, ec)) {
                if (!de.is_regular_file()) continue;
                if (cppExts.find(de.path().extension().string()) == cppExts.end()) continue;
                if (processFile(de.path(), outputDir, scanDir, regen, debug)) ++ok;
                else ++fail;
            }
        } else {
            std::cerr << "[error] Either --scan or --file is required\n";
            return 1;
        }

        std::cout << "Done: " << ok << " ok, " << fail << " failed\n";
        return fail > 0 ? 1 : 0;

    } else if (cmd == "scrub") {
        fs::path scanDir;
        bool debug = false;
        for (int i = 2; i < argc; ++i) {
            std::string a = argv[i];
            if (a == "--scan" && i + 1 < argc) scanDir = argv[++i];
            else if (a == "--debug") debug = true;
        }
        if (scanDir.empty()) {
            std::cerr << "[error] --scan is required for scrub\n";
            return 1;
        }
        std::error_code ec;
        for (const auto& de : fs::recursive_directory_iterator(scanDir, fs::directory_options::skip_permission_denied, ec)) {
            if (!de.is_regular_file()) continue;
            if (de.path().extension() == ".json") scrubFile(de.path(), debug);
        }
        return 0;

    } else {
        std::cerr << "[error] Unknown command: " << cmd << "\n";
        return 1;
    }
}
