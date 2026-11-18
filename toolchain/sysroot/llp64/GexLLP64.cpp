// GexLLP64: clang's wasm64 target with Windows x64's data model (LLP64), for
// Wine's PE side under 64-bit Wine. clang has no
// wasm64 Windows triple; this plugin changes the data model of the
// wasm64-unknown-unknown target before anything is parsed:
//   long 32 bits; long double = double (MSVC); size_t, ptrdiff_t, intptr_t,
//   intmax_t and int64_t are long long; wint_t is unsigned short.
// The predefined macros are computed before plugins run, so llp64.h
// (mkmacros.sh, force-included) redefines them as x86_64 MSVC's. Use with
// -mms-bitfields and -fshort-wchar. A layout test checks the result.
//
//   clang --target=wasm64-unknown-unknown -fplugin=GexLLP64.so -include llp64.h ...
//
// It also gives dllexport and dllimport their PE meaning on a target that
// drops them: a declaration marked with either has default visibility, so a
// DLL exports what MinGW code marks dllexport and refers to what it marks
// dllimport by name (clang's wasm default visibility is hidden, like a PE
// symbol nobody exports). The wasm target knows no dllexport attribute, so
// gexcc-win renames the two (-Ddllexport=__gexw_dllexport,
// -Ddllimport=__gexw_dllimport) to this plugin's attributes, in both
// __declspec(...) and __attribute__((...)) spellings.
#include "clang/AST/ASTConsumer.h"
#include "clang/AST/Attr.h"
#include "clang/Sema/ParsedAttr.h"
#include "clang/Sema/Sema.h"
#include "clang/Basic/TargetInfo.h"
#include "clang/Frontend/CompilerInstance.h"
#include "clang/Frontend/FrontendPluginRegistry.h"
#include "llvm/ADT/APFloat.h"

using namespace clang;

namespace {

// The integer-type choices are protected members of TransferrableTargetInfo;
// a member pointer formed in a derived class reaches them on any TargetInfo.
struct Model : TargetInfo {
  using TI = TransferrableTargetInfo;
  static void apply(TargetInfo &T) {
    T.LongWidth = T.LongAlign = 32;
    T.LongDoubleWidth = T.LongDoubleAlign = 64;
    T.LongDoubleFormat = &llvm::APFloat::IEEEdouble();
    IntType TI::*ll[] = {&Model::SizeType, &Model::IntMaxType, &Model::PtrDiffType, &Model::IntPtrType,
                         &Model::Int64Type};
    for (auto m : ll)
      T.*m = (m == &Model::SizeType) ? UnsignedLongLong : SignedLongLong;
    T.*(&Model::WIntType) = UnsignedShort;
  }
};

struct Action : PluginASTAction {
  std::unique_ptr<ASTConsumer> CreateASTConsumer(CompilerInstance &, llvm::StringRef) override {
    return std::make_unique<ASTConsumer>();
  }
  bool ParseArgs(const CompilerInstance &CI, const std::vector<std::string> &) override {
    const TargetInfo &T = CI.getTarget();
    if (T.getTriple().getArch() != llvm::Triple::wasm64) {
      CI.getDiagnostics().Report(CI.getDiagnostics().getCustomDiagID(
          DiagnosticsEngine::Error, "GexLLP64: the target must be wasm64"));
      return false;
    }
    Model::apply(const_cast<TargetInfo &>(T));
    return true;
  }
  ActionType getActionType() override { return AddBeforeMainAction; }
};

struct DllAttr : ParsedAttrInfo {
  DllAttr() {
    static constexpr Spelling S[] = {{ParsedAttr::AS_Declspec, "__gexw_dllexport"},
                                     {ParsedAttr::AS_GNU, "__gexw_dllexport"},
                                     {ParsedAttr::AS_Declspec, "__gexw_dllimport"},
                                     {ParsedAttr::AS_GNU, "__gexw_dllimport"}};
    Spellings = S;
  }
  bool diagAppertainsToDecl(Sema &, const ParsedAttr &, const Decl *D) const override {
    return isa<FunctionDecl>(D) || isa<VarDecl>(D) || isa<CXXRecordDecl>(D);
  }
  AttrHandling handleDeclAttribute(Sema &S, Decl *D, const ParsedAttr &) const override {
    if (!D->hasAttr<VisibilityAttr>())
      D->addAttr(VisibilityAttr::CreateImplicit(S.Context, VisibilityAttr::Default));
    return AttributeApplied;
  }
};

} // namespace

static ParsedAttrInfoRegistry::Add<DllAttr> Y("gexw-dll", "dllexport and dllimport as default visibility");
static FrontendPluginRegistry::Add<Action> X("gex-llp64", "Windows x64 data model (LLP64) on wasm64");
